# Embedded OrcaSlicer presets

These JSON files are verbatim copies of the system presets of **OrcaSlicer 2.5.1** (tag
`v2.5.1`, commit `eef7710`), `resources/profiles/<maker>/`, for every maker except Bambu Lab
(whose presets come from Bambu Studio, see [../bambu/README.md](../bambu/README.md)), plus
OrcaSlicer's shared filament library (`OrcaFilamentLibrary`). OrcaSlicer and its profiles are
licensed under the **GNU AGPL-3.0**, the same license as only3d.
Upstream: <https://github.com/OrcaSlicer/OrcaSlicer/tree/main/resources/profiles>.

`crates/only3d-profiles` packs every `*.json` under each maker's `machine/`, `process/` and
`filament/` into the binary at build time, with an index, and reads a preset only when it is
used. A preset's `inherits` and `include` resolve within its own maker first, then in the
filament library (see `crates/only3d-profiles/src/store.rs`).

How far only3d is tested on each printer is in [profiles/printers.json](../printers.json) and
[docs/printers.md](../../docs/printers.md); `python3 tools/printers_sweep.py` slices a test box
for every printer and reports any that don't.

Do not edit these files by hand. To refresh them, copy the JSON files of a newer OrcaSlicer's
`resources/profiles` (every folder and `<maker>.json` but `BBL`) and update the version above;
then run `python3 crates/only3d-profiles/tools/gen_orca_defaults.py` on that version's
`src/libslic3r/PrintConfig.cpp` (OrcaSlicer's code defaults, for settings no preset gives).
