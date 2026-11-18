#!/usr/bin/env python3
"""Agent evals for only3d: real Claude agents do scripted tasks through the MCP server,
and the resulting session is checked automatically.

Each task runs in its own throwaway session (`only3d serve` with an empty ONLY3D_HOME,
so no printer is reachable and nothing touches the user's data). Optional `setup` steps seed
the session as "the user". A headless `claude -p` agent gets only only3d's MCP tools (no
printing, no shell), then the session is inspected via `describe` and scored.

    python3 evals/run.py                     # all tasks
    python3 evals/run.py parametric_box      # some tasks
    python3 evals/run.py --model sonnet --max-turns 25

Results: evals/results/<timestamp>.json, plus a table on stdout. Costs real tokens (roughly
$0.15-0.60 per task with the default model).
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# ONLY3D_BIN picks another build (e.g. target/phase2/release/only3d)
BIN = Path(os.environ.get("ONLY3D_BIN") or ROOT / "target" / "release" / "only3d").resolve()
BLOCKED = [
    "print", "printer_control", "printer_set", "printer_ams", "printer_file", "printer_skip_objects",
    "printer_alert", "printer_nozzle", "printer_add", "export", "export_bambu_project",
]


def sock_call(path, tool, args, actor="user"):
    s = socket.socket(socket.AF_UNIX)
    s.connect(path)
    s.sendall((json.dumps({"id": 1, "tool": tool, "args": args, "actor": actor, "client": "evals"}) + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = s.recv(1 << 20)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf)


def score(task, state, answer, project=None):
    data = state.get("data") or {}
    proj = (project or {}).get("data") or {}
    objs = data.get("objects") or []
    params = data.get("params") or {}
    findings = data.get("findings") or []
    results = []
    for chk in task.get("checks", []):
        (name, want), = chk.items()
        ok, why = True, ""
        if name == "objects_min":
            ok, why = len(objs) >= want, f"{len(objs)} objects"
        elif name == "params_include":
            missing = [p for p in want if p not in params]
            ok, why = not missing, f"params {sorted(params)}"
        elif name == "param_equals":
            bad = {k: params.get(k) for k, v in want.items() if not _num_eq(params.get(k), v)}
            ok, why = not bad, f"got {bad}" if bad else "ok"
        elif name == "size_close":
            tol = want.get("tol", 1.0)
            # the biggest visible object is "the part"
            vis = [o for o in objs if not o.get("hidden")]
            part = max(vis, key=lambda o: o.get("volume_mm3", 0), default=None)
            size = part.get("size") if part else None
            if not size:
                ok, why = False, "no object"
            else:
                axes = {"x": size[0], "y": size[1], "z": size[2]}
                off = {a: round(axes[a], 2) for a in ("x", "y", "z") if a in want and abs(axes[a] - want[a]) > tol}
                ok, why = not off, f"size {[round(v, 1) for v in size]}"
        elif name == "checks_clean":
            errs = [f for f in findings if str(f.get("severity", "")).lower() == "error"]
            ok, why = not errs, "; ".join(f.get("message", "") for f in errs) or "no errors"
        elif name == "sliced":
            if not want:
                continue
            sl = data.get("slice") or {}
            ok, why = bool(sl.get("current")), f"slice {sl or 'none'}"
        elif name == "filament_contains":
            fil = [f.get("preset", "") for f in data.get("filaments") or []]
            ok, why = any(want.lower() in f.lower() for f in fil), f"filaments {fil}"
        elif name == "plates":
            # {"count": 2, "names": [...], "min_objects": [6, 6]}
            pl = data.get("plates") or [{"objects": [o.get("id") for o in objs]}]
            bad = []
            if "count" in want and len(pl) != want["count"]:
                bad.append(f"{len(pl)} plates")
            for k, nm in enumerate(want.get("names", [])):
                got = pl[k].get("name") if k < len(pl) else None
                if (got or "").lower() != nm.lower():
                    bad.append(f"plate {k + 1} named {got!r}")
            for k, n in enumerate(want.get("min_objects", [])):
                got = len(pl[k].get("objects") or []) if k < len(pl) else 0
                if got < n:
                    bad.append(f"plate {k + 1} has {got} objects")
            ok, why = not bad, "; ".join(bad) or f"{len(pl)} plates: " + ", ".join(f"{q.get('name')} ({len(q.get('objects') or [])})" for q in pl)
        elif name == "object_flags":
            # {"locked": true}: some object in the project (any plate) has these values
            hits = [o.get("id") for o in proj.get("objects") or [] if all(o.get(k, False) == v for k, v in want.items())]
            ok, why = bool(hits), f"objects {hits}" if hits else f"no object with {want}"
        elif name == "object_filaments":
            # {"includes": [1, 2]}: some object prints with all of these (its own + its parts')
            def fils(o):
                return {o.get("filament", 1)} | {q.get("filament", 1) for q in o.get("parts") or []}
            hits = [o.get("id") for o in proj.get("objects") or [] if set(want["includes"]) <= fils(o)]
            ok, why = bool(hits), f"objects {hits}" if hits else "no object uses " + str(want["includes"])
        elif name == "edges":
            # [[id, "min_x", id2, "max_x", offset]]: id's edge == id2's edge + offset (±0.3 mm)
            def edge(oid, e):
                o = next((x for x in objs if x.get("id") == oid), None)
                if not o:
                    return None
                b = o.get("bounds") or {}
                lo, hi = b.get("min") or [0, 0, 0], b.get("max") or [0, 0, 0]
                k = {"x": 0, "y": 1, "z": 2}[e[-1]]
                return (lo if e.startswith("min") else hi)[k]
            bad = []
            for a_id, a_e, b_id, b_e, off in want:
                va, vb = edge(a_id, a_e), edge(b_id, b_e)
                if va is None or vb is None or abs(va - (vb + off)) > 0.3:
                    bad.append(f"{a_id}.{a_e}={va} vs {b_id}.{b_e}{off:+}={None if vb is None else round(vb + off, 2)}")
            ok, why = not bad, "; ".join(bad) or "all edges match"
        elif name == "specs_min":
            specs = proj.get("specs") or []
            ok, why = len(specs) >= want, f"{len(specs)} saved checks: " + ", ".join(f"{x.get('kind')} {x.get('object')}" for x in specs)
        elif name == "answer_mentions":
            missing = [w for w in want if w.lower() not in (answer or "").lower()]
            ok, why = not missing, f"missing {missing}" if missing else "ok"
        else:
            ok, why = False, f"unknown check {name}"
        results.append({"check": name, "ok": ok, "detail": why})
    return results


def _num_eq(a, b):
    try:
        return abs(float(a) - float(b)) < 1e-6
    except (TypeError, ValueError):
        return a == b


def run_task(task, run_dir, model, max_turns):
    d = run_dir / task["id"]
    (d / "home").mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, ONLY3D_HOME=str(d / "home"), ONLY3D_SOCKET="s.sock")
    serve = subprocess.Popen([str(BIN), "serve"], cwd=d, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # macOS limits socket paths to ~104 bytes: work from the task folder and use a short name
    prev_cwd = os.getcwd()
    os.chdir(d)
    sock = "s.sock"
    try:
        for _ in range(100):
            if os.path.exists(sock):
                break
            time.sleep(0.1)
        for tool, args in task.get("setup", []):
            r = sock_call(sock, tool, args)
            if not r.get("ok"):
                raise RuntimeError(f"setup {tool} failed: {r}")
        (d / "mcp.json").write_text(json.dumps({"mcpServers": {"only3d": {
            "command": str(BIN), "args": ["mcp"], "env": {"ONLY3D_HOME": str(d / "home"), "ONLY3D_SOCKET": "s.sock"}}}}))
        cmd = ["claude", "-p", task["prompt"], "--mcp-config", "mcp.json", "--strict-mcp-config",
               "--allowedTools", "mcp__only3d",
               "--disallowedTools", ",".join([f"mcp__only3d__{t}" for t in BLOCKED] + ["Bash", "Edit", "Write"]),
               "--output-format", "stream-json", "--verbose", "--max-turns", str(max_turns)]
        if model:
            cmd += ["--model", model]
        t0 = time.time()
        p = subprocess.run(cmd, cwd=d, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=900)
        wall = time.time() - t0
        (d / "transcript.jsonl").write_text(p.stdout)
        calls, errors, final = [], [], {}
        for line in p.stdout.splitlines():
            try:
                m = json.loads(line)
            except json.JSONDecodeError:
                continue
            if m.get("type") == "assistant":
                calls += [c["name"].replace("mcp__only3d__", "") for c in m["message"].get("content", []) if c.get("type") == "tool_use"]
            elif m.get("type") == "user":
                for c in m["message"].get("content", []):
                    if c.get("type") == "tool_result" and c.get("is_error"):
                        txt = c.get("content")
                        if isinstance(txt, list):
                            txt = " ".join(x.get("text", "") for x in txt if isinstance(x, dict))
                        errors.append(str(txt)[:200])
            elif m.get("type") == "result":
                final = m
        state = sock_call(sock, "describe", {}, actor="user")
        project = sock_call(sock, "get_project", {}, actor="user")
        checks = score(task, state, final.get("result", ""), project)
        return {
            "task": task["id"], "pass": bool(checks) and all(c["ok"] for c in checks) and not final.get("is_error"),
            "checks": checks, "turns": final.get("num_turns"), "cost_usd": final.get("total_cost_usd"),
            "seconds": round(wall, 1), "tool_calls": len(calls), "tool_errors": errors, "calls": calls,
            "answer": final.get("result", ""), "agent_error": final.get("is_error", True) if final else "no result",
        }
    finally:
        os.chdir(prev_cwd)
        serve.kill()
        serve.wait()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("tasks", nargs="*")
    ap.add_argument("--model")
    ap.add_argument("--max-turns", type=int, default=30)
    a = ap.parse_args()
    if not BIN.exists():
        sys.exit(f"build first: cargo build --release ({BIN} missing)")
    tasks = [json.loads(p.read_text()) for p in sorted((ROOT / "evals" / "tasks").glob("*.json"))]
    if a.tasks:
        tasks = [t for t in tasks if t["id"] in a.tasks]
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = ROOT / "evals" / ".runs" / stamp
    results = []
    for t in tasks:
        print(f"... {t['id']}", flush=True)
        try:
            results.append(run_task(t, run_dir, a.model, a.max_turns))
        except Exception as e:  # keep going: one broken task must not hide the others
            results.append({"task": t["id"], "pass": False, "checks": [], "error": str(e)})
    out = ROOT / "evals" / "results"
    out.mkdir(exist_ok=True)
    # results are committed: keep the runner's home folder (and so its user name) out of them
    text = json.dumps({"model": a.model or "default", "results": results}, indent=1).replace(str(Path.home()), "~")
    (out / f"{stamp}.json").write_text(text)
    print(f"\n| task | pass | checks | turns | tool calls | tool errors | cost | time |\n|---|---|---|---|---|---|---|---|")
    for r in results:
        ok = sum(c["ok"] for c in r.get("checks", []))
        print(f"| {r['task']} | {'yes' if r['pass'] else 'NO'} | {ok}/{len(r.get('checks', []))} | {r.get('turns')} | {r.get('tool_calls')} | {len(r.get('tool_errors', []))} | ${(r.get('cost_usd') or 0):.2f} | {r.get('seconds')}s |")
        for c in r.get("checks", []):
            if not c["ok"]:
                print(f"|   failed: {c['check']}: {c['detail']} |")
        if r.get("error"):
            print(f"|   error: {r['error']} |")
    passed = sum(r["pass"] for r in results)
    print(f"\n{passed}/{len(results)} passed, ${sum((r.get('cost_usd') or 0) for r in results):.2f} total. Details: evals/results/{stamp}.json")


if __name__ == "__main__":
    main()
