# Embedded Bambu Studio presets

These JSON files are verbatim copies of Bambu Lab's system presets from **Bambu Studio**
(vendor bundle `BBL`, version `02.09.00.75`, copied from
`~/Library/Application Support/BambuStudio/system/BBL/`, which is the copy Bambu Studio itself
loads; it is newer than the `02.09.00.74` bundle inside the app). Bambu Studio and its profiles
are licensed under the **GNU AGPL-3.0**, the same license as only3d.
Upstream: <https://github.com/bambulab/BambuStudio/tree/master/resources/profiles/BBL>.

`crates/only3d-profiles` embeds every `*.json` under `machine/`, `process/` and `filament/`
at build time (see `crates/only3d-profiles/build.rs`) and uses them as a fallback when Bambu Studio is
not installed (`ProfileStore::system()` / `ProfileStore::embedded()`).

Contents: the complete resolution closure (every `inherits` parent and every `include`
template) of

| kind     | preset                                                                  |
|----------|-------------------------------------------------------------------------|
| machine  | `Bambu Lab H2D 0.4 nozzle` (+ the 6 `... template *_gcode` includes)    |
| machine  | `Bambu Lab H2D` (machine model: bed model, default materials)           |
| process  | `0.20mm Standard @BBL H2D`                                              |
| filament | `Bambu PLA Basic @BBL H2D` (the 0.4 mm variant has no nozzle suffix)    |
| filament | `Bambu PLA Matte @BBL H2D`                                              |
| filament | `Bambu PETG Basic @BBL H2D 0.4 nozzle`                                  |
| filament | `Bambu ASA @BBL H2D 0.4 nozzle`                                         |

Do not edit these files by hand. To refresh them, copy the same closure again from a newer
Bambu Studio install and update the version above.

## Filament colour names

`filament_colour_names.tsv` lists Bambu's English colour name for every single-colour filament
(filament id, product line, hex, name), taken from Bambu Studio's
`profiles/BBL/filament/filaments_color_codes.json` (same license). only3d uses it to name
spools ("Bambu PETG Translucent Pink", see `crates/only3d-studio/src/spool_names.rs`). Regenerate it
with `python3 tools/bambu-colour-names.py [path/to/filaments_color_codes.json]`.
