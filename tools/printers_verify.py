"""Check only3d's G-code for each printer against Bambu Studio's own command line.

    python3 tools/printers_verify.py                       # every Bambu printer
    python3 tools/printers_verify.py "Bambu Lab X1 Carbon 0.4 nozzle" --update

For each printer preset: a 40 x 40 x 10 mm box, centred, on the printer's default process and
filament (as set_printer picks them), sliced by only3d and by Bambu Studio (macOS: /Applications/BambuStudio.app, or
$BAMBU_STUDIO) with the same presets. Compares the start G-code line by line, the CONFIG block
key by key, and the time and grams. `--update` marks the printer's slicing "checked" in
profiles/printers.json when its start G-code matches exactly, and rewrites docs/printers.md.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ONLY3D = os.path.join(ROOT, "target", "release", "only3d")
DUMP = os.path.join(ROOT, "target", "release", "examples", "dump_preset")
BAMBU = os.environ.get("BAMBU_STUDIO", "/Applications/BambuStudio.app/Contents/MacOS/BambuStudio")
# the H2D Pro, H3C and X2D are newer than some firmware notes; all are in Bambu's bundle
PRINTERS = [
    "Bambu Lab A1 0.4 nozzle", "Bambu Lab A1 mini 0.4 nozzle", "Bambu Lab A2L 0.4 nozzle",
    "Bambu Lab P1P 0.4 nozzle", "Bambu Lab P1S 0.4 nozzle", "Bambu Lab P2S 0.4 nozzle",
    "Bambu Lab X1 0.4 nozzle", "Bambu Lab X1 Carbon 0.4 nozzle", "Bambu Lab X1E 0.4 nozzle", "Bambu Lab X2D 0.4 nozzle",
    "Bambu Lab H2D 0.4 nozzle", "Bambu Lab H2D Pro 0.4 nozzle", "Bambu Lab H2S 0.4 nozzle", "Bambu Lab H2C 0.4 nozzle", "Bambu Lab H3C 0.4 nozzle",
]
# per-job values that legitimately differ between two slicers' runs
JOB_KEYS = {"time", "date", "estimated", "filament_used", "total", "printing_time", "model_printing_time", "first_layer", "nozzle_volume", "wipe_tower", "sequence", "objects_info", "thumbnail"}


def call(work, tool, args):
    env = dict(os.environ, ONLY3D_HOME=os.path.join(work, "home"))
    p = subprocess.run([ONLY3D, "call", tool, json.dumps(args), "-p", os.path.join(work, "p.only3d.json")], env=env, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"{tool}: {(p.stdout + p.stderr).strip()[:300]}")
    return p.stdout


def section(lines, start, end, progress=True):
    """Lines from `start` to `end`; without M73 progress lines when `progress` is false (each
    slicer spreads them by its own time estimate)."""
    try:
        a = next(i for i, l in enumerate(lines) if l.startswith(start))
        b = next(i for i, l in enumerate(lines[a:], a) if l.startswith(end))
    except StopIteration:
        return []
    return [l.rstrip() for l in lines[a:b + 1] if progress or not l.startswith("M73 ")]


def config(lines):
    out = {}
    for l in section(lines, "; CONFIG_BLOCK_START", "; CONFIG_BLOCK_END")[1:-1]:
        if " = " in l:
            k, v = l[2:].split(" = ", 1)
            out[k.strip()] = v.strip()
    return out


def stats(lines):
    t = g = None
    for l in lines[:400]:
        if "model printing time" in l:
            t = l.split("model printing time:")[1].split(";")[0].strip()
        if "total filament weight [g]" in l:
            g = float(l.split(":")[-1])
    return t, g


def hms(text):
    return sum(int(p[:-1]) * {"d": 86400, "h": 3600, "m": 60, "s": 1}[p[-1]] for p in (text or "").split()) or None


def verify(machine, work, shift=0.0):
    os.makedirs(os.path.join(work, "home"), exist_ok=True)
    stl = os.path.join(work, "box.stl")
    call(work, "new_project", {"name": "verify"})
    call(work, "set_printer", {"machine": machine})
    bed = re.search(r"bed (\d+)x(\d+) mm", call(work, "describe", {}))
    cx, cy = (int(bed.group(1)) / 2, int(bed.group(2)) / 2) if bed else (128, 128)
    call(work, "add_object", {"id": "box", "shape": {"type": "box", "size": [40, 40, 10]}, "position": [cx + shift, cy]})
    # Bambu's command line puts a single filament on the first nozzle (the H2D's left), where
    # only3d and Bambu Studio's window pick the main one: compare like with like
    if "nozzle 1" in call(work, "describe", {}) or machine.split(" 0.")[0].endswith(("H2D", "H2D Pro", "H2C", "X2D")):
        call(work, "set_filament", {"slot": 1, "nozzle": "left"})
    # Bambu Studio gets the same box as an STL (it arranges it to the plate's centre too)
    call(work, "export", {"path": stl})
    proj = json.load(open(os.path.join(work, "p.only3d.json")))
    process, filament = proj["process"]["preset"], proj["filaments"][0]["preset"]
    env = dict(os.environ, ONLY3D_HOME=os.path.join(work, "home"))
    o3d = os.path.join(work, "o3d.gcode")
    p = subprocess.run([ONLY3D, "slice", os.path.join(work, "p.only3d.json"), "-o", o3d], env=env, capture_output=True, text=True)
    o3d_error = None if p.returncode == 0 else f"only3d: {(p.stdout + p.stderr).strip()[:300]}"
    # which nozzle only3d printed with, from its G-code's filament_map (1-based)
    o3d_lines = open(o3d, errors="replace").read().splitlines() if o3d_error is None else []
    fm = config(o3d_lines).get("filament_map")
    ext = int(fm.split(",")[0]) - 1 if fm and fm.split(",")[0].strip().isdigit() else None
    presets = {}
    info = {"machine": machine, "process": process, "filament": filament}
    for kind, name in (("machine", machine), ("process", process), ("filament", filament)):
        d = json.loads(subprocess.run([DUMP, kind, name, "--embedded"], capture_output=True, text=True, check=True).stdout)
        if kind == "machine":
            info["name"] = d.get("printer_model") or machine.split(" 0.")[0]
            info["nozzles"] = len(d.get("nozzle_diameter") or [0])
        d.pop("inherits", None)
        d["from"] = "system"
        if kind == "process":
            d["curr_bed_type"] = proj["printer"]["bed_type"]
            # a two-nozzle printer: the filament on the nozzle only3d chose (Bambu's command line
            # otherwise takes the first, the H2D's left)
            if ext is not None:
                d["filament_map"] = [str(ext + 1)]
        presets[kind] = os.path.join(work, f"{kind}.json")
        json.dump(d, open(presets[kind], "w"))
    out = os.path.join(work, "bbl")
    os.makedirs(out, exist_ok=True)
    subprocess.run(["perl", "-e", "alarm 300; exec @ARGV", BAMBU, "--datadir", os.path.join(work, "bbldata"), "--arrange", "1", "--orient", "0", "--slice", "0",
                    "--outputdir", out, "--load-settings", f"{presets['machine']};{presets['process']}", "--load-filaments", presets["filament"], "box.stl"],
                   cwd=work, capture_output=True, text=True)
    bbl = os.path.join(out, "plate_1.gcode")
    # keep both for later (gen_bambu_options.py --add-missing reads Bambu's)
    keep = os.path.join(ROOT, "target", "readiness", "printers", machine.replace(" ", "_"))
    os.makedirs(keep, exist_ok=True)
    for src, name in ((o3d, "only3d.gcode"), (bbl, "bambu.gcode")):
        if os.path.exists(src):
            shutil.copy(src, os.path.join(keep, name))
    if o3d_error:
        return dict(info, error=o3d_error)
    if not os.path.exists(bbl):
        return dict(info, error="Bambu Studio wrote no G-code")
    a, b = open(o3d, errors="replace").read().splitlines(), open(bbl, errors="replace").read().splitlines()
    sa = section(a, "; EXECUTABLE_BLOCK_START", "; MACHINE_START_GCODE_END", progress=False)
    sb = section(b, "; EXECUTABLE_BLOCK_START", "; MACHINE_START_GCODE_END", progress=False)
    differ = [(x, y) for x, y in zip(sa, sb) if x != y]
    ca, cb = config(a), config(b)
    keys = [k for k in cb if k in ca and ca[k] != cb[k] and not any(j in k for j in JOB_KEYS)]
    missing = [k for k in cb if k not in ca]
    # Bambu sometimes arranges the box off-centre (on some two-nozzle machines): then only the
    # bed-probing area differs. Put only3d's box where Bambu's went and compare once more.
    if shift == 0.0 and differ and all(x.lstrip().startswith("G29") and y.lstrip().startswith("G29") for x, y in differ):
        gx = lambda l: float(re.search(r" X(-?[0-9.]+)", l).group(1))
        dx = gx(differ[0][1]) - gx(differ[0][0])
        again = verify(machine, tempfile.mkdtemp(prefix="o3d-verify-"), shift=dx)
        again["placement_shift_mm"] = dx
        return again
    (ta, ga), (tb, gb) = stats(a), stats(b)
    return {
        **info,
        "start_lines": len(sb), "start_same": len(sb) == len(sa) and not differ,
        "start_diffs": len(differ) + abs(len(sa) - len(sb)), "start_examples": differ[:4],
        "config_diffs": keys, "config_missing": missing,
        "time": [ta, tb], "grams": [ga, gb],
        "time_ratio": round(hms(ta) / hms(tb), 3) if hms(ta) and hms(tb) else None,
    }


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    printers = args or PRINTERS
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "only3d", "-p", "only3d-profiles", "--example", "dump_preset"], cwd=ROOT, check=True)
    results = []
    for m in printers:
        work = tempfile.mkdtemp(prefix="o3d-verify-")
        try:
            r = verify(m, work)
        except Exception as e:  # keep going: one printer must not hide the others
            r = {"machine": m, "error": str(e)[:300]}
        results.append(r)
        if "error" in r:
            print(f"{m}: ERROR {r['error']}")
        else:
            print(f"{m}: start G-code {'identical' if r['start_same'] else str(r['start_diffs']) + ' lines differ'} of {r['start_lines']}; "
                  f"config {len(r['config_diffs'])} values differ, {len(r['config_missing'])} keys missing; "
                  f"time {r['time'][0]} vs {r['time'][1]}, grams {r['grams'][0]} vs {r['grams'][1]}")
    os.makedirs(os.path.join(ROOT, "target", "readiness"), exist_ok=True)
    path = os.path.join(ROOT, "target", "readiness", f"printers-{time.strftime('%Y-%m-%d')}.json")
    json.dump(results, open(path, "w"), indent=1)
    print(f"(details: {os.path.relpath(path, ROOT)})")
    if "--update" in sys.argv:
        update_registry(results)


def update_registry(results):
    path = os.path.join(ROOT, "profiles", "printers.json")
    reg = json.load(open(path))
    by_machine = {p["machine"]: p for p in reg["printers"]}
    today = time.strftime("%Y-%m-%d")
    for r in results:
        if "error" in r:
            continue
        p = by_machine.get(r["machine"])
        if p is None:
            # a printer of Bambu's bundle that isn't listed yet: it slices, nobody has checked it
            name = r.get("name") or r["machine"].split(" 0.")[0]
            p = {"id": re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-").replace("bambu-lab-", "bambu-"), "name": name, "maker": "Bambu Lab", "machine": r["machine"],
                 "nozzles": r.get("nozzles", 1), "slice": {"status": "untested"}, "export": {"status": "untested"}, "print": {"status": "untested"}}
            reg["printers"].append(p)
            by_machine[r["machine"]] = p
        timing = f"time and grams {r['time'][0]} / {r['grams'][0]} g vs Bambu's {r['time'][1]} / {r['grams'][1]} g"
        # a note someone wrote by hand (more than this script checks) stays
        generated = not p["slice"].get("note") or p["slice"]["note"].startswith(("Start G-code identical", "Compared with"))
        if not generated:
            continue
        if r["start_same"] and p["slice"]["status"] in ("untested", "checked"):
            p["slice"] = {"status": "checked", "note": f"Start G-code identical to Bambu Studio's command line on {r['process']} and {r['filament']}; {timing}", "date": today}
        elif not r["start_same"] and p["slice"]["status"] == "untested":
            ex = "; ".join(f"`{a}` where Bambu writes `{b}`" for a, b in r["start_examples"][:2])
            p["slice"] = {"status": "untested", "note": f"Compared with Bambu Studio's command line on {r['process']}: {r['start_diffs']} of {r['start_lines']} start G-code lines differ ({ex}); {timing}", "date": today}
    order = {m: i for i, m in enumerate(PRINTERS)}
    reg["printers"].sort(key=lambda p: (p["maker"], order.get(p["machine"], len(order)), p["name"]))
    json.dump(reg, open(path, "w"), indent=2, ensure_ascii=False)
    open(path, "a").write("\n")
    subprocess.run([sys.executable, os.path.join(ROOT, "tools", "printers_table.py")], check=True)


if __name__ == "__main__":
    main()
