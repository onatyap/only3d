"""only3d's performance numbers for the readiness scorecard (docs/readiness.md).

    python3 tools/readiness/bench.py [--bin target/release/only3d] [--no-gui] [--no-bambu]

Slices the reference models (the example tray, the first-print tile and a generated 1M-triangle
organic mesh) with only3d and, when it is installed, with Bambu Studio's command line on the
same presets. Then it times a 1M-triangle import, MCP tool calls, the window's startup and its
idle CPU, and the GPU viewport benchmark. Prints a Markdown table; writes JSON next to it in
target/readiness/.
"""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time

try:
    import resource  # not on Windows: CPU time and memory are left out there
except ImportError:
    resource = None

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BAMBU_MAC = "/Applications/BambuStudio.app/Contents/MacOS/BambuStudio"
PRESETS = {
    "machine": "Bambu Lab H2D 0.4 nozzle",
    "process": "0.20mm Standard @BBL H2D",
    "filament": "Bambu PLA Basic @BBL H2D",
}


def run(cmd, env=None, cwd=None, timeout=600, stdin=None):
    """Run cmd; return (seconds, child CPU seconds, peak memory in MB, completed process)."""
    if resource is None:
        t = time.perf_counter()
        p = subprocess.run(cmd, env=env, cwd=cwd, timeout=timeout, input=stdin, capture_output=True, text=True)
        return time.perf_counter() - t, 0.0, 0.0, p
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    t = time.perf_counter()
    p = subprocess.run(cmd, env=env, cwd=cwd, timeout=timeout, input=stdin, capture_output=True, text=True)
    wall = time.perf_counter() - t
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    # macOS reports bytes, Linux kilobytes; the peak is over every child so far, so it is
    # only meaningful when the largest run comes last, or per fresh interpreter
    peak = after.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024)
    return wall, cpu, peak, p


def env_with(home, **extra):
    e = dict(os.environ)
    e["ONLY3D_HOME"] = home
    e.update(extra)
    return e


def only3d_slice(bin_, model, work, home):
    out = os.path.join(work, os.path.basename(model) + ".gcode")
    wall, cpu, _, p = run([bin_, "slice", model, "-o", out, "--json"], env=env_with(home))
    if p.returncode != 0:
        return {"error": (p.stderr or p.stdout).strip().splitlines()[0][:200]}
    s = json.loads(p.stdout)["stats"]
    return {"wall_s": round(wall, 2), "cpu_s": round(cpu, 2), "print_s": round(s["print_time_s"]), "grams": round(s["filament_g"], 2), "layers": s["layers"]}


def flatten_presets(work):
    """Bambu's presets as flat JSON files for its command line (see dump_preset)."""
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "only3d-profiles", "--example", "dump_preset"], cwd=ROOT, check=True)
    dump = os.path.join(ROOT, "target", "release", "examples", "dump_preset")
    paths = {}
    for kind, name in PRESETS.items():
        p = subprocess.run([dump, kind, name, "--embedded"], capture_output=True, text=True, check=True)
        d = json.loads(p.stdout)
        d.pop("inherits", None)
        d["from"] = "system"
        if kind == "process":
            d["curr_bed_type"] = "Textured PEI Plate"
        paths[kind] = os.path.join(work, kind + ".json")
        with open(paths[kind], "w") as f:
            json.dump(d, f)
    return paths


def bambu_slice(exe, stl, work, presets):
    out = tempfile.mkdtemp(dir=work)
    data = tempfile.mkdtemp(dir=work)
    cmd = [exe, "--datadir", data, "--arrange", "1", "--orient", "0", "--slice", "0", "--outputdir", out,
           "--load-settings", f"{presets['machine']};{presets['process']}", "--load-filaments", presets["filament"], stl]
    wall, cpu, _, p = run(cmd, cwd=work, timeout=900)
    g = os.path.join(out, "plate_1.gcode")
    if not os.path.exists(g):
        return {"error": f"Bambu Studio exited {p.returncode} without G-code"}
    grams = print_s = None
    with open(g, errors="replace") as f:
        for line in f:
            if "total filament weight [g]" in line:
                grams = float(line.split(":")[-1])
            if "model printing time" in line:
                print_s = hms(line.split("model printing time:")[1].split(";")[0])
            if grams is not None and print_s is not None:
                break
    return {"wall_s": round(wall, 2), "cpu_s": round(cpu, 2), "print_s": print_s, "grams": grams}


def hms(text):
    total = 0
    for part in text.split():
        n, unit = int(part[:-1]), part[-1]
        total += n * {"d": 86400, "h": 3600, "m": 60, "s": 1}[unit]
    return total


def mcp_latency(bin_, model, home, calls=300):
    """Milliseconds per `describe` round trip over stdio MCP, one call at a time like an agent
    (no window), and the size of the tool list an agent loads."""
    def msg(i, method, params=None):
        m = {"jsonrpc": "2.0", "method": method}
        if i is not None:
            m["id"] = i
        if params is not None:
            m["params"] = params
        return json.dumps(m) + "\n"
    p = subprocess.Popen([bin_, "mcp", "--no-live"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env_with(home))
    p.stdin.write(msg(0, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "bench", "version": "1"}}))
    p.stdin.write(msg(None, "notifications/initialized"))
    p.stdin.write(msg(1, "tools/list"))
    p.stdin.write(msg(2, "tools/call", {"name": "open_project", "arguments": {"path": model}}))
    p.stdin.flush()
    p.stdout.readline()
    tools_chars = len(p.stdout.readline())
    p.stdout.readline()
    t = time.perf_counter()
    for i in range(calls):
        p.stdin.write(msg(3 + i, "tools/call", {"name": "describe", "arguments": {}}))
        p.stdin.flush()
        p.stdout.readline()
    each = (time.perf_counter() - t) / calls
    p.stdin.close()
    p.wait(timeout=30)
    return {"describe_ms": round(each * 1000, 2), "tools_list_chars": tools_chars, "ok": p.returncode == 0}

def gui_startup(bin_, model, work, home):
    shot = os.path.join(work, "startup.png")
    e = env_with(home, ONLY3D_GUI_SHOT_FRAMES="2", ONLY3D_GUI_SHOT_DELAY_MS="0")
    wall, _, _, p = run([bin_, "gui", model, "--no-live", "--screenshot", shot], env=e, timeout=120)
    return {"to_first_screenshot_s": round(wall, 2)} if p.returncode == 0 and os.path.exists(shot) else {"error": (p.stderr or "no screenshot")[-200:]}


def gui_idle_cpu(bin_, model, home, seconds=10):
    if sys.platform == "win32":
        return {"skipped": "not measured on Windows"}
    proc = subprocess.Popen([bin_, "gui", model, "--no-live"], env=env_with(home), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(6)
        a = cputime(proc.pid)
        time.sleep(seconds)
        b = cputime(proc.pid)
        return {"cpu_s_per_10s": round((b - a) * 10 / seconds, 2)}
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def cputime(pid):
    t = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    parts = t.replace("-", ":").split(":")
    secs = float(parts[-1])
    mult = 60
    for p in reversed(parts[:-1]):
        secs += float(p) * mult
        mult *= 60
    return secs


def viewport():
    p = subprocess.run(["cargo", "test", "--release", "-q", "-p", "only3d-gui", "viewport_frame_time", "--", "--nocapture"], cwd=ROOT, capture_output=True, text=True)
    for line in (p.stdout + p.stderr).splitlines():
        if "mesh 1M" in line or "skipping" in line:
            return {"line": line.strip()}
    return {"error": "no result"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(ROOT, "target", "release", "only3d" + (".exe" if sys.platform == "win32" else "")))
    ap.add_argument("--no-gui", action="store_true")
    ap.add_argument("--no-bambu", action="store_true")
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="only3d-bench-")
    home = os.path.join(work, "home")
    os.makedirs(home)
    blob = os.path.join(work, "blob.stl")
    subprocess.run([sys.executable, "-I", os.path.join(ROOT, "tools", "readiness", "make_blob.py"), blob, "710"], check=True, capture_output=True)
    models = {}
    for name in ["japandi_tray", "first_print_tile"]:
        models[name] = shutil.copy(os.path.join(ROOT, "examples", f"{name}.only3d.json"), work)
    models["blob_1M"] = blob

    r = {"platform": f"{platform.system()} {platform.machine()}", "date": time.strftime("%Y-%m-%d"), "slice": {}}
    for name, m in models.items():
        r["slice"][name] = {"only3d": only3d_slice(a.bin, m, work, home)}
    bambu = os.environ.get("BAMBU_STUDIO") or (BAMBU_MAC if os.path.exists(BAMBU_MAC) else None)
    if bambu and not a.no_bambu:
        presets = flatten_presets(work)
        # Bambu's command line takes meshes: export the projects as STL first
        for name in ["japandi_tray", "first_print_tile"]:
            stl = os.path.join(work, name + ".stl")
            subprocess.run([a.bin, "call", "export", json.dumps({"path": stl}), "-p", models[name]], env=env_with(home), capture_output=True, check=True)
            r["slice"][name]["bambu"] = bambu_slice(bambu, stl, work, presets)
        r["slice"]["blob_1M"]["bambu"] = bambu_slice(bambu, blob, work, presets)
    wall, _, _, p = run([a.bin, "info", blob])
    r["import_1M_s"] = round(wall, 2)
    r["mcp"] = mcp_latency(a.bin, models["japandi_tray"], home)
    if not a.no_gui:
        r["gui_startup"] = gui_startup(a.bin, models["japandi_tray"], work, home)
        r["gui_idle"] = gui_idle_cpu(a.bin, models["japandi_tray"], home)
        r["viewport"] = viewport()

    os.makedirs(os.path.join(ROOT, "target", "readiness"), exist_ok=True)
    out = os.path.join(ROOT, "target", "readiness", f"bench-{platform.system().lower()}-{r['date']}.json")
    with open(out, "w") as f:
        json.dump(r, f, indent=2)

    print(f"### Benchmarks: {r['platform']}, {r['date']}\n")
    print("| Model | only3d slice | Bambu Studio slice | only3d estimate | Bambu estimate |")
    print("|---|---|---|---|---|")
    for name, v in r["slice"].items():
        o, b = v["only3d"], v.get("bambu", {})
        fmt = lambda x: f"{x['wall_s']} s ({x['cpu_s']} s CPU)" if "wall_s" in x else x.get("error", "–")
        est = lambda x: f"{round(x['print_s'] / 60, 1)} min, {x['grams']} g" if x.get("print_s") else "–"
        print(f"| {name} | {fmt(o)} | {fmt(b)} | {est(o)} | {est(b)} |")
    print(f"\n- 1M-triangle STL import and check: {r['import_1M_s']} s")
    print(f"- MCP `describe`: {r['mcp']['describe_ms']} ms per call; tools/list {r['mcp']['tools_list_chars']} characters")
    if not a.no_gui:
        print(f"- Window to first screenshot: {r['gui_startup']}")
        print(f"- Idle window: {r['gui_idle']}")
        print(f"- Viewport: {r['viewport']}")
    print(f"\n(JSON: {os.path.relpath(out, ROOT)})")


if __name__ == "__main__":
    main()
