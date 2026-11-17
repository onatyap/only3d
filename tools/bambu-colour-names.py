#!/usr/bin/env python3
"""Regenerate profiles/bambu/filament_colour_names.tsv from Bambu Studio's colour table.

Usage: python3 tools/bambu-colour-names.py [path/to/filaments_color_codes.json]

The default source is the copy inside Bambu Studio on macOS. Only single-colour entries are
kept (gradients and multi-colour spools have no one hex to match).
"""
import json
import sys

src = sys.argv[1] if len(sys.argv) > 1 else "/Applications/BambuStudio.app/Contents/Resources/profiles/BBL/filament/filaments_color_codes.json"
data = json.load(open(src, encoding="utf-8"))["data"]
rows = set()
for e in data:
    cols = e.get("fila_color") or []
    if len(cols) != 1:
        continue
    hexv = cols[0].lstrip("#").upper()[:6]
    name = (e.get("fila_color_name") or {}).get("en", "").strip()
    if len(hexv) != 6 or not name:
        continue
    rows.add((e["fila_id"], e["fila_type"].strip(), hexv, name))
out = "profiles/bambu/filament_colour_names.tsv"
with open(out, "w", encoding="utf-8") as f:
    f.write("# Bambu Lab filament colour names (single-colour spools), from Bambu Studio's\n")
    f.write("# profiles/BBL/filament/filaments_color_codes.json (AGPL-3.0). Regenerate with\n")
    f.write("# tools/bambu-colour-names.py. Columns: filament id, product line, hex, English name.\n")
    for r in sorted(rows):
        f.write("\t".join(r) + "\n")
print(f"{len(rows)} colours -> {out}")
