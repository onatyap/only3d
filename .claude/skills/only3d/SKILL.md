---
name: only3d
description: Use when designing 3D-printable parts, slicing them, or running a Bambu Lab printer (H2D) through only3d's MCP tools (mcp__only3d__*): modelling, printability checks, slicing for time/grams, printing with the user's approval, watching a job, handling printer alerts, filament spools and print history.
---

# Working with only3d

only3d is a parametric modelling, slicing and printing studio built for a Bambu Lab H2D.
Every action is an MCP tool. Your calls go to the only3d window the user has open, labelled
"agent", so the user sees the part change as you work. If none is open, your first call opens
one. Each window shows a short session id (like `k7m2`) on its agent link chip: with several
open you get `session_choice_needed`, so ask the user which one and call `session` with its id.
The first result tells you which window you are in; tell the user. Don't work headless
(`session {"headless": true}`) unless the user asks for that: they would see nothing.

## The loop that works

1. **Look first.** Call `describe`: objects, params, what the user selected, checks, last slice.
   `get_selection` tells you what "this" means when the user says "make this taller".
2. **Parameters, not magic numbers.** Put every size the user may tweak into `set_params` and
   use expressions in the shape tree (`"wall*2"`). Sizes are in millimetres, Z points up, and
   primitives are centred on X/Y and sit on Z=0.
3. **Model** with `add_object` and `update_object`. Shapes are JSON trees:
   - primitives: box, cylinder, sphere, torus, extrude, revolve, import;
   - combinations: union, difference, intersection, hull;
   - patterns: array, polar;
   - edges: corner_radius, chamfer_*, fillet_* on box and cylinder;
   - placement: `at`, `rotate`, `scale`, `mirror` on any node.
4. **Verify before you claim success.**
   - `check` for printability: overhangs, thin walls, open edges, off the plate.
   - `section` at the heights that matter: wall thickness, hole sizes.
   - `render` and actually look at the picture.
5. **Slice** (`set_filament`, `set_process`, then `slice`) for time and grams. `settings_catalog`
   lists every setting, with units and ranges.
6. **Report:** what you built, its params, and any decisions left for the user.

## Functional parts

- **Shapes beyond primitives:** `slot` (an obround), `tube`, `polygon` (sides with `d` or
  `flats`, for hex nuts and pencils), `wedge` (a ramp), `text` (raised letters; subtract it to
  engrave), and `extrude` with `corner_radius`.
- **`shell`** hollows a box, cylinder, slot, polygon or extrude, open at the top unless `closed`.
  Use it for trays, cups and enclosures.
- **`hole`** is a cutter for a `difference`.
  - Sizes come from a screw (`screw`: "M3") or a plain `d`.
  - `kind`: clearance, tap, countersink, counterbore, insert (heat-set) or nut (hex trap).
  - It goes down `depth` from its `at`. Put that point on the surface.
  - `axis` "x"/"y" makes a horizontal teardrop that prints without support.
  - Holes get the calibrated fit automatically.
- **`split_object`** cuts a part along a plane (x/y/z) when it's too big, or to print it flat.
  `update_object {"mirror": "x"}` mirrors.
- **Design checks belong to the project:**
  `check {"add": {"kind": "wall", "object": "cup", "z": 20, "min": 1.6, "note": "…"}}`.
  The kinds are size (`axis`), clearance (`other`, as placed), wall (`z`) and overhang, each
  with min/max. They rerun on every change and show in the window's Checks panel. Add them for
  what the user cares about.
- **Assemblies:** `assembly` places printed objects as they go together (each in the frame
  it was modelled in: `{"object": id, "at": [x, y, z], "rotate": [x, y, z]}`, expressions
  allowed) and adds bought hardware as `reference` shapes (`{"name", "shape", "color"}`) that
  are never printed. Nothing moves on the plates. It lists overlaps between bodies and returns an
  iso picture. Other views and a cutaway: `render {"assembly": true, "cut": {"axis": "y"}}`.
- **Per-object settings** (`update_object {"settings": …}`) take only what the slicer applies
  per object (`settings_catalog` marks them `per_object`: walls, infill, speeds, brim,
  supports). A brim is `brim_width` (mm), not `brim_type`. Bad keys and values fail at once.
- **Fit calibration:**
  1. `fit {"action": "coupon"}` adds a test bar and a pin to print.
  2. Once the user has tried the pin in each hole,
     `fit {"action": "record", "slip": 0.2, "press": 0.1}`.
  3. From then on, holes on that printer and filament get the slip value.

## Multi-colour objects

An object can have `parts`: extra bodies in other filaments that move, turn and scale with it,
such as a text inlay, a coloured ring, or the pink centre of a flower. Pass them to `add_object`
or `update_object`: `[{"name": "ring", "shape": {...}, "filament": 2}]`, in the same coordinates
as `shape`. `[]` removes them. Multi-colour Bambu 3MF files import with their parts.

`combine` joins objects where they stand, like Bambu Studio's assemble and boolean tools:
`assemble` makes one object whose bodies keep their filaments; `union`, `difference` (the
first id minus the rest) and `intersection` make one solid; `explode` splits an assembly back
into objects. `update_object {"spool": "<spool id>"}` prints an object with an inventory spool
(it reuses the slot holding that spool, or adds one).

The native slicer prints them: `slice` reports each filament's grams, the filament changes and
the prime tower.

Push/Pull a flat face (SketchUp's Push/Pull, Fusion's Press Pull):
`update_object {"id": "box", "push_pull": {"at": [x, y, z], "distance": 5}}`. `at` is a point
on the face in plate mm (a face centre from `describe` or `measure` works); a positive
distance pulls the face out (adds material in its outline), a negative one pushes it in
(cuts). The far side stays where it was. It is saved as a `push_pull` step wrapping the shape,
so it follows later moves and turns, and `undo` takes it back. Curved surfaces are made of
small flat facets, so pull flat faces only.

## Arranging

`arrange` packs the active plate by default. With `op` it places `ids` against an anchor: an
object id, `"plate"`, or by default the last of several ids.
- `align`: `x` left | center | right, `y` front | middle | back.
- `dock`: put objects beside the anchor, with a `gap`.
- `distribute`: equal gaps along an `axis`; with `gap` it stacks them in order, touching at 0.
- `grid`, `circle`, `swap`, `snap` (to plate cells).
- `match_size`: `mode` match | stretch | fill.
- `copy_transform`.

Front is the plate's front edge (y = 0), left is x = 0, whatever the camera shows.

`duplicate_object` puts copies in the nearest free space and moves nothing else. With
`direction` (+x, -x, +y, -y) or `grid` [rows, cols] and a `gap`, it lays them out in a row or a
matrix from the original. `offset` [x, y] puts copy k at k steps from the original, exactly
there. `fill: true` adds as many as fit on the plate. Use `arrange` only when the user wants the
whole plate re-packed.

New objects and imports land clear of what is already on the plate. A multi-object file keeps
its layout as one block. An import's `position` is where the user wants it; if something is
there it moves to the nearest free spot, and with no room left it goes onto a new plate. The
output says so (`placed` in its data). Nothing that was already there moves.

## Plates

A project can have several build plates, like Bambu Studio's.
- `plates` lists them: names, the objects on each, and which one is active.
- `plate` does the rest: `add` (with a name; the new plate becomes active), `select`, `rename`,
  `duplicate`, `remove` (objects go to `to`, or are deleted), and `move` (put `ids` on a plate).
- New objects land on the active plate. `slice`, `check`, `print`, `arrange` and `render` act on
  the active plate, and `describe` details it while listing the others.
- Tools that name an object (`update_object`, `measure`, `select`) switch to that object's
  plate. `slice {"plate": 2}` slices plate 2.
- A batch that doesn't fit on one plate goes on several: one plate each, arranged.

## Downloaded models

`import_file {"repair": true}` closes holes, turns faces outward and removes duplicate or
degenerate faces as the file loads. The file itself isn't changed. The import says when an
object isn't watertight.

Adapt a model with a `difference` or `union` around its `import` node: holes, hooks and text
all work that way. Use `split_object` to cut it to fit the plate.

STEP files (`.step`, `.stp`) import as solids, one object per solid, converted to millimetres
from inches or metres.

## OpenSCAD

A `.scad` import renders through the user's OpenSCAD. Its result lists the file's customizer
variables with their ranges. Change them with `set_params {"object": "<id>", "params":
{"width": 60}}` rather than editing the file. A number may be an expression over the project's
parameters (`"wall*2"`), so it follows them. `null` puts a variable back to the file's value.
Only customizer variables can be set: top-level ones outside `/* [Hidden] */`.

`export {"path": "x.scad"}` writes the active plate as OpenSCAD: project parameters become
variables, each object a module, placed as on the plate. Shapes OpenSCAD has no match for
(Push/Pull steps, meshes from STEP or 3MF objects) go in as polyhedra, or STL files beside the
.scad when big. The result names them.

## Finding files

`workspace` lists the user's workspace folder like a file explorer: folders, only3d projects,
models, print files and images. Pass `path` to go into a subfolder. Open projects with
`open_project`, and bring models in with `import_file`.

`update_object {"locked": true}` keeps an object in place: the window won't drag it, and
`arrange` leaves it where it is.

Edits are undoable (`undo`). Every error has a `code` and a `hint`; follow the hint.

## Printing: the user decides

- `print` without `confirm` is a dry run. It returns the checklist plus photos of the model and
  the plate. Fix whatever fails first.
- `print` with `confirm: true` *asks*. The user sees a dialog in only3d showing what will
  print, the plate photo and the checklist, and approves or denies it. The same goes for
  stopping a print, skipping objects, deleting printer files, loading or unloading filament,
  switching nozzles, answering alerts, heating the nozzle or bed, and drying filament (turning
  a heater off or stopping drying doesn't ask).
- A plate with several filaments, on a printer where that isn't verified yet (the H2D for now),
  says so in the preflight (`[!]`, `multi_filament`) and needs `multi_filament_ok: true` as
  well. Tell the user what it says first; set it only if they still want the print.
- `denied_by_user` means: don't retry, ask what they want instead. `approval_required` means
  there is no only3d window to ask in, so ask the user to open one (`only3d`).
- Only request a print when the user wants one.
- Several filaments print natively. On the H2D each filament gets a nozzle: the one its spool
  feeds (left holder → left, right holder and AMS → right), or set it with `set_filament
  nozzle`. Filaments on the same nozzle change through the AMS and flush in the chute. A prime
  tower takes each change. `print` maps every filament to its tray (spool location, else the
  loaded tray of the same material and nearest colour, or pass `trays`) and refuses a tray
  wired to the other nozzle.
- `print` without confirm lists the printer's trays (A1..D4, ext left/right) with each spool's
  name, colour and what's left. When the nearest loaded tray is another product line or colour
  than the project's, it asks instead of ticking it (`needs_choice`), and a confirmed print
  stops with `filament_choice_needed`: show the user the trays, ask which, then pass `tray` (or
  `trays`). Each row of its `mapping` lists `choices`: every tray holding that material, best
  first, with what differs. A fatal or serious HMS error on the printer stops it until cleared on the screen.
- Supports slice natively: `set_process overrides {"support": true}` builds tree supports (the
  Bambu presets' `support_type`) or, with `"support_type": "normal"`, straight columns. For the
  cleanest underside put a support filament in another slot and set
  `"support_interface_filament": <slot>`: the roof prints in it with no gap (a prime tower too).

## React instead of polling

`wait_for_event` waits for the next thing that happens and returns `next_since` for your next
call. It covers:
- `project_changed` and `selection_changed`, with who did it (user or agent);
- `sliced` and `approval`;
- the printer: `printer_state`, `printer_stage` (heating, leveling, purifying the air...),
  `printer_alert`, `print_progress`, `print_finished`, `print_failed`, `printer_connection`;
- the AMS:
  - `ams_drying`: drying starts, finishes (`finished: true`), is stopped early (`finished:
    false`, with the time it had left) or fails;
  - `ams_slot`: a spool is `inserted`, `removed`, `read` (with what the AMS found) or
    `changed` (filament set on the printer's screen). The live window's inventory follows
    on its own; call `spool_sync` to see what it registered.

Filter with `kinds`. It never blocks the user's window.

## Printer alerts

`printer_alert` lists the current alerts with Bambu's own text and the actions Bambu Studio
offers for each. Typical examples:
- spaghetti detected: resume or stop;
- nozzle mismatch: resume (problem solved), or ignore and resume.

Explain the alert, take a `printer_camera` photo, and propose the offered action. The user
approves it.

## H2D facts

- `printer_status` has an AMS section per unit: model (AMS 2 Pro...), humidity, temperature and
  `drying` (state, filament, °C, time left, or `blocked_by` reasons). A slot the AMS is reading
  shows "reading…". `ams_state.read_rfid_on_insert: false` means new Bambu spools aren't read
  when inserted (the AMS setting "Insertion update").
- Two 0.4 mm nozzles. In the slicer, extruder 0 is the left nozzle; in firmware, T0 is the
  right one.
- The AMS feeds the right nozzle. External spool holder 254 is the left one and 255 the right.
- The plate is 350 x 320 mm. The left nozzle reaches x 0–325 and the right nozzle x 25–350.
- Timelapses: `print` with `timelapse: true` records one (only when the user wants it).
  `printer_files {"timelapses": 3}` lists the videos with the newest 3 thumbnails, and `printer_file`
  downloads one to `save_to`.
- The camera serves one viewer at a time, and a snapshot takes about 2–3 s. Don't hammer it.
- After a job the printer can spend a few minutes purifying the chamber air while still
  RUNNING at 100%. The `printer_stage` event says so.

## Filament and history

- `spool_list` and `spool_sync` read the AMS. RFID spools are tracked from the AMS's own
  readings. Spools in slots the AMS reports empty go to the shelf.
- `spool_sync` notices say what needs the user: `unregistered` (register it, or put the shelf
  spool it names there), `wrong_spool` (move the filed spool), `unmeasured` (a used RFID spool
  counted as full: try `read_rfid`, else ask the user to weigh it and `spool_weigh`).
- `printer_ams {"action": "read_rfid", "slot": n}` makes the AMS turn a spool, read its tag and
  measure what's left (~15 s, then an `ams_slot` event). It needs filament at the slot's inlet
  (`slot_empty` otherwise: ask the user to push the end in) and an empty nozzle on that AMS.
- `printer_ams {"action": "dry", "ams_id": 0}` plans drying an AMS 2 Pro (45–65 °C) or AMS HT
  (45–85 °C), 1–72 h. Temperature and hours default to Bambu's preset for the loaded filament
  that needs the lowest temperature; pass `filament_type`, `dry_temp`, `dry_hours` to change
  them. Show the user the plan, then repeat with `confirm: true`; they approve it in only3d.
  `filament_would_soften` means another loaded filament's end would deform at that temperature:
  dry cooler, or ask the user to pull those ends out of the inlets. `stop_drying` stops it.
- To register a pile or an order, use `spool_add` with `quantity` for identical spools and
  `gross_g` for a used spool on the scale. Give the brand and line; after the first spool of a
  line, its empty-spool weight, price and preset are filled in. File Bambu spools as brand
  "Bambu Lab" and leave them off the AMS slots: `spool_sync` gives each its RFID tag when it is
  loaded.
- `print_detail` traces a print to its project version, its objects and the grams used.
- `usage_report` and `inventory_query` (read-only SQL) answer "how much PETG did I use".
- `print_delete` removes a record that never charged filament, such as a test export. Ask the
  user first.

## Workflows

The server offers prompts for the common journeys: `design_part`, `print_plate`,
`diagnose_print` and `watch_print`. In Claude Code they are `/mcp__only3d__<name>`.
