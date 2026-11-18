"""Compare only3d's supports with Bambu Studio's command line on a few test parts.

    python3 tools/supports_verify.py                 # tree supports, every part
    python3 tools/supports_verify.py --grid          # grid ("normal") supports
    python3 tools/supports_verify.py --none          # no supports (the baseline)
    python3 tools/supports_verify.py hook sphere     # some parts only
    python3 tools/supports_verify.py model.stl       # a model file

Each part (a T, an arch, an overhanging hook, a sphere; also a table and a torus) goes on the H2D's default process and
filament with supports on, sliced by only3d and by Bambu Studio (macOS:
/Applications/BambuStudio.app, or $BAMBU_STUDIO) with the same presets. Prints the time,
grams and support extrusion (mm of support and interface lines) of each.
"""

import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ONLY3D = os.path.join(ROOT, "target", "release", "only3d")
DUMP = os.path.join(ROOT, "target", "release", "examples", "dump_preset")
BAMBU = os.environ.get("BAMBU_STUDIO", "/Applications/BambuStudio.app/Contents/MacOS/BambuStudio")
MACHINE = "Bambu Lab H2D 0.4 nozzle"

box = lambda sx, sy, sz, x=0, y=0, z=0: {"type": "box", "size": [sx, sy, sz], "at": [x, y, z]}
PARTS = {
    # a post with a wide slab on top: a flat overhang all around
    "t": {"type": "union", "children": [box(4, 4, 10), box(30, 10, 2, z=10)]},
    # two pillars and a deck: a bridge 30 mm long, 15 mm up
    "arch": {"type": "union", "children": [box(10, 10, 15, x=-20), box(10, 10, 15, x=20), box(50, 10, 5, z=15)]},
    # a post, an arm out at the top and a lip hanging down from its end: branches under the arm
    # must find their way past the lip
    "hook": {"type": "union", "children": [box(6, 10, 30, x=-12), box(30, 10, 4, z=26), box(4, 10, 10, x=13, z=16)]},
    # a ball resting on the plate: the lower half overhangs more and more towards the bottom
    "sphere": {"type": "sphere", "d": 30},
    # a table: a wide top 40 mm up on four legs, a thousand tips
    "table": {"type": "union", "children": [box(6, 6, 40, x=-37, y=-37), box(6, 6, 40, x=37, y=-37), box(6, 6, 40, x=-37, y=37), box(6, 6, 40, x=37, y=37), box(80, 80, 3, z=40)]},
    # a ring lying flat: slopes all round, outside and in the hole
    "torus": {"type": "torus", "major": 20, "minor": 6},
}


name_path = {}  # model files given on the command line


def call(work, tool, args):
    env = dict(os.environ, ONLY3D_HOME=os.path.join(work, "home"))
    p = subprocess.run([ONLY3D, "call", tool, json.dumps(args), "-p", os.path.join(work, "p.only3d.json")], env=env, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"{tool}: {(p.stdout + p.stderr).strip()[:300]}")
    return p.stdout


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


def features(lines):
    """mm of extruded lines per feature, and the layers that have support."""
    out, feat, x, y, z, layers = {}, None, None, None, 0.0, set()
    for l in lines:
        if l.startswith("; FEATURE:"):
            feat = l.split(":", 1)[1].strip()
        elif l.startswith("; Z_HEIGHT:"):
            z = float(l.split(":")[1])
        code = l.split(";")[0].strip()
        if not code.startswith(("G1 ", "G0 ", "G2 ", "G3 ")):
            continue
        nx, ny, e = x, y, 0.0
        for t in code.split()[1:]:
            k, v = t[0], t[1:]
            try:
                v = float(v)
            except ValueError:
                continue
            if k == "X":
                nx = v
            elif k == "Y":
                ny = v
            elif k == "E":
                e = v
        if e > 0 and x is not None and nx is not None and feat:
            out[feat] = out.get(feat, 0.0) + ((nx - x) ** 2 + (ny - y) ** 2) ** 0.5
            if feat.startswith("Support"):
                layers.add(round(z, 2))
        x, y = nx, ny
    return out, len(layers)


def run(name, shape, kind, work):
    os.makedirs(os.path.join(work, "home"), exist_ok=True)
    call(work, "new_project", {"name": name})
    call(work, "set_printer", {"machine": MACHINE})
    if shape is None:
        call(work, "import_file", {"path": os.path.abspath(name_path[name]), "id": "part", "position": [175, 160]})
    else:
        call(work, "add_object", {"id": name, "shape": shape, "position": [175, 160]})
    call(work, "set_filament", {"slot": 1, "nozzle": "left"})
    call(work, "set_process", {"overrides": {"support": kind != "none", "support_type": "normal" if kind == "grid" else "tree"}})
    stl = os.path.join(work, "part.stl")
    call(work, "export", {"path": stl})
    proj = json.load(open(os.path.join(work, "p.only3d.json")))
    env = dict(os.environ, ONLY3D_HOME=os.path.join(work, "home"))
    o3d = os.path.join(work, "o3d.gcode")
    p = subprocess.run([ONLY3D, "slice", os.path.join(work, "p.only3d.json"), "-o", o3d], env=env, capture_output=True, text=True)
    res = {"part": name}
    if p.returncode != 0:
        res["only3d_error"] = (p.stdout + p.stderr).strip()[:300]
    presets = {}
    for which, preset in (("machine", MACHINE), ("process", proj["process"]["preset"]), ("filament", proj["filaments"][0]["preset"])):
        d = json.loads(subprocess.run([DUMP, which, preset, "--embedded"], capture_output=True, text=True, check=True).stdout)
        d.pop("inherits", None)
        d["from"] = "system"
        if which == "process":
            d["curr_bed_type"] = proj["printer"]["bed_type"]
            d["enable_support"] = "0" if kind == "none" else "1"
            d["support_type"] = "normal(auto)" if kind == "grid" else "tree(auto)"
        presets[which] = os.path.join(work, f"{which}.json")
        json.dump(d, open(presets[which], "w"))
    out = os.path.join(work, "bbl")
    os.makedirs(out, exist_ok=True)
    subprocess.run(["perl", "-e", "alarm 300; exec @ARGV", BAMBU, "--datadir", os.path.join(work, "bbldata"), "--arrange", "1", "--orient", "0", "--slice", "0",
                    "--outputdir", out, "--load-settings", f"{presets['machine']};{presets['process']}", "--load-filaments", presets["filament"], "part.stl"],
                   cwd=work, capture_output=True, text=True)
    for who, path in (("only3d", o3d), ("bambu", os.path.join(out, "plate_1.gcode"))):
        if not os.path.exists(path):
            continue
        lines = open(path, errors="replace").read().splitlines()
        t, g = stats(lines)
        f, nl = features(lines)
        res[who] = {"time": t, "s": hms(t), "g": g, "support_mm": round(f.get("Support", 0.0)), "interface_mm": round(f.get("Support interface", 0.0)), "support_layers": nl}
    return res


def main():
    kind = "grid" if "--grid" in sys.argv else "none" if "--none" in sys.argv else "tree"
    names = [a for a in sys.argv[1:] if not a.startswith("--")] or ["t", "arch", "hook", "sphere"]
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "only3d"], cwd=ROOT, check=True)
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "only3d-profiles", "--example", "dump_preset"], cwd=ROOT, check=True)
    print(f"{kind} supports, {MACHINE}")
    for n in names:
        if n.lower().endswith((".stl", ".3mf", ".obj", ".step")):
            name_path[os.path.basename(n)] = n
            n = os.path.basename(n)
        work = tempfile.mkdtemp(prefix="o3d-sup-")
        r = run(n, PARTS.get(n), kind, work)
        a, b = r.get("only3d", {}), r.get("bambu", {})
        ratio = lambda k: f"{a[k] / b[k]:.2f}" if a.get(k) and b.get(k) else "-"
        print(f"{n:7} only3d {a.get('time')} {a.get('g')} g (support {a.get('support_mm')} mm, interface {a.get('interface_mm')} mm) | "
              f"bambu {b.get('time')} {b.get('g')} g (support {b.get('support_mm')} mm, interface {b.get('interface_mm')} mm) | "
              f"time x{ratio('s')}, grams x{ratio('g')}  [{work}]" + (f"  ERROR {r['only3d_error']}" if "only3d_error" in r else ""))


if __name__ == "__main__":
    main()
