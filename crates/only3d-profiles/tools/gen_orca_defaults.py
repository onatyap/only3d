#!/usr/bin/env python3
"""Write crates/only3d-profiles/data/orca_code_defaults.json from OrcaSlicer's PrintConfig.cpp.

OrcaSlicer's printer templates may read a setting that no preset sets (`filament_notes`,
`enable_pressure_advance`): OrcaSlicer then uses the option's code default. This collects the
defaults that are plain values (numbers, bools, strings, points and lists of them); enums and
computed defaults are left out. only3d adds them to the template context where nothing else
gives the variable a value.

Usage (from the repository root, after `sh tools/fetch-upstream.sh orcaslicer`):

    python3 crates/only3d-profiles/tools/gen_orca_defaults.py \\
        third_party/upstream/orcaslicer/src/libslic3r/PrintConfig.cpp
"""
import json
import re
import sys

ADD = re.compile(r'def\s*=\s*this->add\("([a-z0-9_]+)",\s*(co[A-Za-z]+)\)')
DEFAULT = re.compile(r"set_default_value\(\s*new\s+(ConfigOption[A-Za-z]+)(?:<[^>]*>)?\s*([({])(.*?)[)}]\s*\)\s*;", re.S)


def atom(s):
    s = s.strip()
    if s.startswith('"') and s.endswith('"'):
        return s[1:-1]
    if s in ("true", "false"):
        return "1" if s == "true" else "0"
    m = re.fullmatch(r"-?\d+(\.\d*)?(e-?\d+)?f?", s)
    if m:
        v = s.rstrip("f")
        return v[:-1] if v.endswith(".") else v
    m = re.fullmatch(r"Vec2d\(\s*(-?[\d.]+)\s*,\s*(-?[\d.]+)\s*\)", s)
    if m:
        return f"{float(m.group(1)):g}x{float(m.group(2)):g}"
    return None


def parse(kind, body):
    body = body.strip().strip("{}").strip()
    if kind in ("ConfigOptionFloatOrPercent", "ConfigOptionFloatsOrPercents"):
        m = re.fullmatch(r"\{?\s*(-?[\d.]+)\s*,\s*(true|false)\s*\}?", body)
        if not m:
            return None
        v = atom(m.group(1))
        v = v + "%" if m.group(2) == "true" else v
        return [v] if kind.endswith("Percents") else v
    if kind in ("ConfigOptionPercent",):
        v = atom(body)
        return None if v is None else v + "%"
    plural = kind.endswith("s") and kind not in ("ConfigOptionBools",) or kind == "ConfigOptionBools"
    if plural:
        if body == "":
            return []
        parts = [atom(p) for p in re.split(r",(?![^(]*\))", body)]
        return None if any(p is None for p in parts) else parts
    return atom(body)


def main():
    src = open(sys.argv[1], encoding="utf-8").read()
    adds = list(ADD.finditer(src))
    out = {}
    for i, m in enumerate(adds):
        key, co = m.group(1), m.group(2)
        if co.startswith("coEnum"):
            continue
        end = adds[i + 1].start() if i + 1 < len(adds) else len(src)
        d = DEFAULT.search(src, m.end(), end)
        if not d:
            continue
        v = parse(d.group(1), d.group(3))
        if v is not None:
            out[key] = {"type": co, "value": v}
    path = "crates/only3d-profiles/data/orca_code_defaults.json"
    with open(path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, ensure_ascii=False, sort_keys=True)
        f.write("\n")
    print(f"{len(out)} of {len(adds)} options have a plain default: {path}")


if __name__ == "__main__":
    main()
