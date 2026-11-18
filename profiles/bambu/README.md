# Embedded Bambu Studio presets

These JSON files are verbatim copies of Bambu Lab's system presets from **Bambu Studio**
(vendor bundle `BBL`, version `02.09.00.75`, copied from
`~/Library/Application Support/BambuStudio/system/BBL/`, which is the copy Bambu Studio itself
loads; it is newer than the `02.09.00.74` bundle inside the app). Bambu Studio and its profiles
are licensed under the **GNU AGPL-3.0**, the same license as only3d.
Upstream: <https://github.com/bambulab/BambuStudio/tree/master/resources/profiles/BBL>.

`crates/only3d-profiles` packs every `*.json` under `machine/`, `process/` and `filament/` into
a compressed archive at build time (see `crates/only3d-profiles/build.rs`) and uses it as a
fallback when Bambu Studio is not installed (`ProfileStore::system()` /
`ProfileStore::embedded()`).

Contents: the whole `BBL` bundle, so every Bambu Lab printer with its processes and filaments
(`BBL.json` lists the printer models). Which printers only3d has been checked and printed on is
in [profiles/printers.json](../printers.json) and [docs/printers.md](../../docs/printers.md).

Do not edit these files by hand. To refresh them, copy `machine/`, `process/`, `filament/` and
`BBL.json` from a newer Bambu Studio and update the version above.

## Filament colour names

`filament_colour_names.tsv` lists Bambu's English colour name for every single-colour filament
(filament id, product line, hex, name), taken from Bambu Studio's
`profiles/BBL/filament/filaments_color_codes.json` (same license). only3d uses it to name
spools ("Bambu PETG Translucent Pink", see `crates/only3d-studio/src/spool_names.rs`). Regenerate it
with `python3 tools/bambu-colour-names.py [path/to/filaments_color_codes.json]`.
