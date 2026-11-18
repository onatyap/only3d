#!/usr/bin/env python3
"""Regenerate only3d's Bambu config tables from Bambu Studio sources.

Bambu printers read the G-code CONFIG block, so only3d writes it the way Bambu Studio does
(GCode::append_full_config): every option of the full print config, sorted, serialized by type.

Usage (from the repository root, after `sh tools/fetch-upstream.sh bambustudio`):

    python3 crates/only3d-profiles/tools/gen_bambu_options.py \
        third_party/upstream/bambustudio/src/libslic3r/PrintConfig.cpp \
        <reference .gcode written by Bambu Studio for an H2D> \
        <project_settings.config written by only3d for the same job>

Or, to add only the code defaults a newer Bambu Studio writes and only3d doesn't yet (keeping
the ones already there):

    python3 crates/only3d-profiles/tools/gen_bambu_options.py --add-missing \
        <PrintConfig.cpp of that version> <.gcode Bambu Studio wrote> <.gcode only3d wrote, same job>

Writes
  crates/only3d-profiles/src/option_types.rs   option name -> Bambu ConfigOptionType
  crates/only3d-profiles/data/bambu_code_defaults.json
      values Bambu Studio fills in from code defaults (no preset sets them), taken from the
      reference CONFIG block, minus the per-job keys only3d computes itself.
"""
import json
import re
import sys

# keys only3d computes per job (job.rs / bambu.rs), never taken from the snapshot
COMPUTED = {
    "filament_map", "filament_map_2", "filament_nozzle_map", "filament_map_mode", "filament_self_index",
    "filament_ids", "nozzle_volume_type", "extruder_nozzle_volume_type", "extruder_nozzle_stats",
    "extruder_nozzle_count", "extruder_nozzle_diameter", "extruder_nozzle_slot", "flush_volumes_matrix",
    "different_settings_to_system", "inherits_group", "print_compatible_printers",
    "compatible_machine_expression_group",
}


def option_types(src):
    # drop // comments: some definitions are commented out (e.g. adaptive_layer_height)
    src = "\n".join(line.split("//", 1)[0] for line in src.split("\n"))
    types = {}
    for k, t in re.findall(r'(?:this->)?add(?:_nullable)?\(\s*"([A-Za-z0-9_]+)"\s*,\s*(co[A-Za-z]+)\s*\)', src):
        types.setdefault(k, t)
    # filament overrides Bambu generates from key lists, typed like the machine/process key
    for lst in ("filament_extruder_override_keys", "filament_overhang_override_keys"):
        m = re.search(lst + r"\s*=\s*\{(.*?)\};", src, re.S)
        for key in re.findall(r'"([a-z0-9_]+)"', m.group(1) if m else ""):
            raw = key[len("filament_"):]
            if raw in types:
                types.setdefault(key, types[raw])
    # options Bambu defines in a loop over the axes (`"machine_max_speed_" + axis.name`)
    for prefix in ("machine_max_speed_", "machine_max_acceleration_", "machine_max_jerk_"):
        for axis in "xyze":
            types.setdefault(prefix + axis, "coFloats")
    return types


def config_block(path):
    t = open(path, encoding="utf-8").read()
    s, e = t.index("; CONFIG_BLOCK_START"), t.index("; CONFIG_BLOCK_END")
    out = {}
    for line in t[s:e].split("\n"):
        m = re.match(r"; (\S+) = (.*)$", line)
        if m:
            out[m.group(1)] = m.group(2)
    return out


def unescape(s):
    return s.replace("\\n", "\n").replace("\\r", "\r").replace('\\"', '"').replace("\\\\", "\\")


def parse_strings(s):
    """Inverse of Bambu's escape_strings_cstyle."""
    out, cur, i, quoted = [], "", 0, False
    while i < len(s):
        c = s[i]
        if quoted:
            if c == "\\" and i + 1 < len(s):
                cur += {"n": "\n", "r": "\r"}.get(s[i + 1], s[i + 1])
                i += 2
                continue
            if c == '"':
                quoted = False
            else:
                cur += c
        elif c == '"':
            quoted = True
        elif c == ";":
            out.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    out.append(cur)
    return out


def to_json(t, v):
    """A serialized CONFIG value -> Bambu preset JSON (string or list of strings)."""
    if t == "coStrings":
        return parse_strings(v) if v != "" else []
    if t == "coPointsGroups":
        return v.split("#")
    if t.endswith("s") and t not in ("coString",):
        return v.split(",") if v != "" else []
    if t == "coString":
        return unescape(v)
    return v


def main():
    add_missing = "--add-missing" in sys.argv
    src, ref_gcode, ours_settings = [a for a in sys.argv[1:] if a != "--add-missing"][:3]
    types = option_types(open(src, encoding="utf-8").read())
    names = {
        "coFloat": "Float", "coFloats": "Floats", "coInt": "Int", "coInts": "Ints", "coString": "String",
        "coStrings": "Strings", "coPercent": "Percent", "coPercents": "Percents", "coFloatOrPercent": "FloatOrPercent",
        "coFloatsOrPercents": "FloatsOrPercents", "coPoint": "Point", "coPoints": "Points", "coPoint3": "Point3",
        "coBool": "Bool", "coBools": "Bools", "coEnum": "Enum", "coEnums": "Enums", "coPointsGroups": "PointsGroups",
    }
    rows = "\n".join(f'    ("{k}", OptType::{names[t]}),' for k, t in sorted(types.items()) if t in names)
    rs = f"""//! Bambu Studio option types (generated by tools/gen_bambu_options.py from PrintConfig.cpp; do not
//! edit by hand). Used to serialize the G-code CONFIG block exactly like Bambu Studio.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptType {{
    Float,
    Floats,
    Int,
    Ints,
    String,
    Strings,
    Percent,
    Percents,
    FloatOrPercent,
    FloatsOrPercents,
    Point,
    Points,
    Point3,
    Bool,
    Bools,
    Enum,
    Enums,
    PointsGroups,
}}

/// Sorted by name (binary search).
pub static OPTION_TYPES: &[(&str, OptType)] = &[
{rows}
];
"""
    open("crates/only3d-profiles/src/option_types.rs", "w").write(rs)

    ref = config_block(ref_gcode)
    if add_missing:
        ours = config_block(ours_settings)
        path = "crates/only3d-profiles/data/bambu_code_defaults.json"
        defaults = json.load(open(path, encoding="utf-8"))
        added = []
        for k, v in sorted(ref.items()):
            if k in ours or k in defaults or k in COMPUTED or k not in types:
                continue
            defaults[k] = to_json(types[k], v)
            added.append(k)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(defaults, f, indent=1, ensure_ascii=False, sort_keys=True)
            f.write("\n")
        print(f"{len(types)} option types; added {len(added)} code defaults: {', '.join(added)}")
        return
    ours = json.load(open(ours_settings, encoding="utf-8"))
    defaults = {}
    for k, v in sorted(ref.items()):
        if k in ours or k in COMPUTED or k not in types:
            continue
        defaults[k] = to_json(types[k], v)
    import os
    os.makedirs("crates/only3d-profiles/data", exist_ok=True)
    with open("crates/only3d-profiles/data/bambu_code_defaults.json", "w", encoding="utf-8") as f:
        json.dump(defaults, f, indent=1, ensure_ascii=False, sort_keys=True)
        f.write("\n")
    print(f"{len(types)} option types, {len(defaults)} code defaults")


if __name__ == "__main__":
    main()
