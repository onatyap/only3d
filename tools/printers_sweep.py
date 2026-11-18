"""Slice a test box for every printer only3d knows and look for trouble in the G-code.

    python3 tools/printers_sweep.py                 # every printer (about 1,200 presets)
    python3 tools/printers_sweep.py Creality Voron  # presets whose name contains any of these
    python3 tools/printers_sweep.py --update        # also list the makers' printers in profiles/printers.json

Unlike tools/printers_verify.py this needs no reference slicer: each printer preset gets a
new project on its own default process and filament, a 40 x 40 x 10 mm box in the middle of
its plate, and a slice. A printer passes when the slice succeeds, only3d's G-code check passes,
no template text is left unrendered, no Bambu-only command reaches a printer of another maker,
and the box weighs and takes about what a box should. Results go to
target/readiness/sweep-<date>.json.

`--update` adds every printer model that passes to profiles/printers.json as "untested" for
slicing (it slices, but nobody has compared it with its maker's slicer or printed it) and
rewrites docs/printers.md. Entries already there keep their status.
"""

import concurrent.futures as cf
import json
import os
import re
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ONLY3D = os.path.join(ROOT, "target", "release", "only3d")
# commands only Bambu Lab printers understand
# commands only3d itself writes for Bambu Lab printers (a maker's own templates may use others)
BAMBU_ONLY = re.compile(r"^M981\b|^;VT\d")


def run(args, env, timeout=120):
    p = subprocess.run(args, env=env, capture_output=True, text=True, timeout=timeout)
    return p.returncode, (p.stdout + p.stderr)


def presets(env):
    code, out = run([ONLY3D, "call", "list_presets", json.dumps({"kind": "machine"}), "--json", "-p", os.path.join(env["ONLY3D_HOME"], "list.only3d.json")], env)
    if code != 0:
        sys.exit(f"list_presets failed: {out[:300]}")
    return json.loads(out)["data"]


def check(machine):
    work = tempfile.mkdtemp(prefix="o3d-sweep-")
    env = dict(os.environ, ONLY3D_HOME=os.path.join(work, "home"))
    proj = os.path.join(work, "p.only3d.json")
    calls = [
        {"tool": "new_project", "args": {"name": "sweep"}},
        {"tool": "set_printer", "args": {"machine": machine}},
        {"tool": "add_object", "args": {"id": "box", "shape": {"type": "box", "size": [40, 40, 10]}}},
    ]
    r = {"machine": machine}
    try:
        code, out = run([ONLY3D, "call", "batch", json.dumps({"calls": calls}), "-p", proj], env)
        if code != 0:
            r["error"] = "setup: " + out.strip()[:300]
            return r
        m = re.search(r"(?:^|: |; )process (.+?) -> (.+?)(?:;|$)", out, re.M)
        r["process"] = m.group(2).strip() if m else "0.20mm Standard @BBL H2D"
        gc = os.path.join(work, "out.gcode")
        code, out = run([ONLY3D, "slice", proj, "-o", gc], env)
        if code != 0:
            r["error"] = "slice: " + out.strip()[:300]
            return r
        m = re.search(r"est\. (\d+)h(\d+)m, ([0-9.]+) g", out)
        if m:
            r["minutes"] = int(m.group(1)) * 60 + int(m.group(2))
            r["grams"] = float(m.group(3))
        r["lint_ok"] = "G-code check passed" in out
        problems = []
        if not r["lint_ok"]:
            problems.append("G-code check: " + (re.search(r"G-code check[^\n]*", out) or [""])[0][:200])
        bambu = machine.startswith("Bambu Lab")
        with open(gc, errors="replace") as f:
            for n, line in enumerate(f, 1):
                code_part = line.split(";", 1)[0]
                if ("[" in code_part or "{" in code_part or "}" in code_part) and len(problems) < 8:
                    problems.append(f"line {n}: unrendered template text: {line.strip()[:120]}")
                if not bambu and BAMBU_ONLY.match(line) and len(problems) < 8:
                    problems.append(f"line {n}: Bambu-only command: {line.strip()[:80]}")
        g = r.get("grams")
        if g is not None and not 3.0 <= g <= 20.0:
            problems.append(f"box weighs {g} g")
        # no process of its own fitted (it kept the one it came from)
        if not bambu and (r.get("process") or "").endswith("@BBL H2D"):
            problems.append(f"no process of its own: kept {r['process']}")
        # a 0.05 mm layer on a 0.25 mm nozzle takes hours; ten is wrong
        if r.get("minutes", 0) > 600:
            problems.append(f"box takes {r['minutes']} min")
        r["problems"] = problems
        return r
    except subprocess.TimeoutExpired:
        r["error"] = "timed out"
        return r
    finally:
        subprocess.run(["rm", "-rf", work])


def main():
    names = [a for a in sys.argv[1:] if not a.startswith("--")]
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "only3d"], cwd=ROOT, check=True)
    home = tempfile.mkdtemp(prefix="o3d-sweep-home-")
    env = dict(os.environ, ONLY3D_HOME=home)
    machines = [m for m in presets(env) if not names or any(n.lower() in m.lower() for n in names)]
    print(f"slicing a box for {len(machines)} printer presets ...", flush=True)
    t0 = time.time()
    with cf.ThreadPoolExecutor(max_workers=max(2, (os.cpu_count() or 4) - 2)) as pool:
        results = list(pool.map(check, machines))
    bad = [r for r in results if r.get("error") or r.get("problems")]
    print(f"{len(results) - len(bad)} of {len(results)} pass ({time.time() - t0:.0f} s)")
    for r in bad:
        print(f"- {r['machine']}: {r.get('error') or '; '.join(r['problems'])}")
    os.makedirs(os.path.join(ROOT, "target", "readiness"), exist_ok=True)
    path = os.path.join(ROOT, "target", "readiness", f"sweep-{time.strftime('%Y-%m-%d')}.json")
    json.dump(results, open(path, "w"), indent=1)
    print(f"(details: {os.path.relpath(path, ROOT)})")
    if "--update" in sys.argv:
        update_registry(results)


_MACHINES = None


def orca_machines():
    """(vendor, preset name) -> (vendor, JSON) of every OrcaSlicer machine preset."""
    global _MACHINES
    if _MACHINES is None:
        _MACHINES = {}
        root = os.path.join(ROOT, "profiles", "orca")
        for v in os.listdir(root):
            for d, _, files in os.walk(os.path.join(root, v, "machine")):
                for f in files:
                    if f.endswith(".json"):
                        try:
                            j = json.loads(open(os.path.join(d, f), encoding="utf-8-sig").read())
                        except ValueError:
                            continue
                        _MACHINES[j.get("name", f[:-5])] = (v, j)
    return _MACHINES


def model_of(machine):
    """The printer model a preset is for: its `printer_model` (inherited within its maker),
    else its name without the nozzle size."""
    ms = orca_machines()
    cur, seen = ms.get(machine), set()
    while cur and cur[1].get("name") not in seen:
        seen.add(cur[1].get("name"))
        if cur[1].get("printer_model"):
            return cur[1]["printer_model"]
        cur = ms.get(cur[1].get("inherits", ""))
    return re.sub(r"\s*\(?\s*(HF)?\d+(\.\d+)?\s*(mm)?\s*[Nn]ozzle\)?.*$", "", machine).strip() or machine


def update_registry(results):
    path = os.path.join(ROOT, "profiles", "printers.json")
    reg = json.load(open(path))
    known = {p["machine"] for p in reg["printers"]}
    known_names = {p["name"] for p in reg["printers"]}
    today = time.strftime("%Y-%m-%d")
    # one entry per model: its 0.4 mm nozzle preset when there is one
    by_model = {}
    for r in results:
        if r.get("error") or r.get("problems") or r["machine"].startswith("Bambu Lab"):
            continue
        model = model_of(r["machine"])
        cur = by_model.get(model)
        if cur is None or (" 0.4 nozzle" in r["machine"] and " 0.4 nozzle" not in cur["machine"]):
            by_model[model] = r
    # models none of whose presets slice: listed as not supported, with why
    failing = {}
    for r in results:
        if r["machine"].startswith("Bambu Lab"):
            continue
        model = model_of(r["machine"])
        if model not in by_model and (r.get("error") or r.get("problems")):
            failing.setdefault(model, r)
    added = 0
    for model, r in sorted(failing.items()):
        if model in known_names:
            continue
        why = (r.get("error") or "; ".join(r.get("problems") or [])).split("\n")[0][:160]
        reg["printers"].append({
            "id": re.sub(r"[^a-z0-9]+", "-", model.lower()).strip("-"),
            "name": model, "maker": maker_of(r["machine"]), "machine": r["machine"],
            "slice": {"status": "no", "note": f"Doesn't slice yet: {why}", "date": today},
            "export": {"status": "no"},
            "print": {"status": "no"},
        })
        added += 1
    for model, r in sorted(by_model.items()):
        if r["machine"] in known or model in known_names:
            continue
        maker = maker_of(r["machine"])
        reg["printers"].append({
            "id": re.sub(r"[^a-z0-9]+", "-", model.lower()).strip("-"),
            "name": model, "maker": maker, "machine": r["machine"],
            "slice": {"status": "untested", "note": f"Slices a test box ({r.get('grams')} g, {r.get('minutes')} min on {r.get('process')}) with OrcaSlicer's profile; not yet compared with OrcaSlicer", "date": today},
            "export": {"status": "untested"},
            "print": {"status": "untested"},
        })
        added += 1
    reg["printers"].sort(key=lambda p: (p["maker"] != "Bambu Lab", p["maker"].lower(), p["name"].lower()) if p["maker"] != "Bambu Lab" else (False, "", ""))
    json.dump(reg, open(path, "w"), indent=2, ensure_ascii=False)
    open(path, "a").write("\n")
    print(f"added {added} printer models to profiles/printers.json")
    subprocess.run([sys.executable, os.path.join(ROOT, "tools", "printers_table.py")], check=True)


def maker_of(machine):
    """The maker, from OrcaSlicer's vendor folder the preset lives in."""
    if machine in orca_machines():
        return orca_machines()[machine][0]
    for v in sorted(os.listdir(os.path.join(ROOT, "profiles", "orca")), key=len, reverse=True):
        d = os.path.join(ROOT, "profiles", "orca", v, "machine")
        if os.path.exists(os.path.join(d, machine + ".json")):
            return v
    for v in os.listdir(os.path.join(ROOT, "profiles", "orca")):
        d = os.path.join(ROOT, "profiles", "orca", v, "machine")
        if os.path.isdir(d):
            for f in os.listdir(d):
                try:
                    if json.load(open(os.path.join(d, f), encoding="utf-8-sig")).get("name") == machine:
                        return v
                except Exception:
                    pass
    return machine.split()[0]


if __name__ == "__main__":
    main()
