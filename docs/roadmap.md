# only3d roadmap

Draft for agreement, 2026-11-15. It is built from `docs/backlog.md`, which holds the friction items
F-*, the benchmark capabilities C-* and the debts, and from `docs/design/transforms.md`.

## How it is ordered

1. **Fix daily friction first.** These items are cheap and you hit them every session.
2. **Build foundations that unlock many items:** plates, the object tree and the arrange system.
3. **Then the differentiators,** the things no benchmarked tool does well: fit calibration from
   real prints, project checks that run after every change, and modelling that gives a printable
   part in minutes.
4. **Agents keep parity throughout.** Every feature is a tool first, gets an eval task, and the
   GUI calls the same tool.

## Definition of done (every item)

- A tool for the feature exists in `tools.rs` / `printer.rs`. The GUI and agents both call it.
- Tests and clippy pass, and the GUI is checked with a screenshot.
- At least one eval task covers it, and the suite still passes.
- Anything that touches the printer is verified on the H2D, with your approval where it is gated.
- **Each phase ends with one of your real projects done in only3d**, start to finish.

## Phase 0: stabilise (about 1–2 days)

| Item | What | Size |
|---|---|---|
| Debt | **Commit everything to git** in a clean series of commits, and tag a baseline | S |
| F-15 | Fix the UI-scale slider fighting itself: apply on release | S |
| F-01 | Remove the "you + agents · live" header | S |
| F-02 | Activity rows: hover highlights the object, click selects it or shows the details | S–M |
| F-10 | FRONT and BACK printed on the plate | S |
| F-14 | One primary action at the bottom right of the viewport: Slice → Print… → progress | S |
| F-09 | Plates with several filaments fall back to Bambu Studio's slicer instead of failing | S–M |
| F-12 | Imported Bambu objects come in one colour. **Cause:** they are multi-part (white petals, pink parts) and only3d has no parts yet; the window hid the warning. Phase 0 shows the warning and corrects its text; parts themselves come with F-04 | S |
| Debt | Remove the six diagnostic "exported" print records (with your OK) | S |

**Done when:** none of the listed frictions reproduce, and the work is committed.

**Status, 2026-11-15: done.**
- Committed as `baseline`, then the Phase 0 commit on top.
- The UI scale applies when you let go.
- The activity header is gone. Rows highlight their objects on hover; a click selects the objects
  and opens the full result.
- FRONT and BACK are drawn on the plate.
- One main button at the bottom right: Slice plate → time and grams with Print… → printer
  progress.
- Plates with several filaments get "Open in Bambu Studio": the project is written next to
  yours and opened there, because Bambu Studio's command line can't slice them.
- The import shows an amber warning when colours are lost (real fix: parts, F-04).
- New `print_delete` tool; the six test records are deleted, and ids are never reused.

## Phase 1: plates, workspace, objects and arranging (about 3 weeks)

| Item | What | Size |
|---|---|---|
| F-03 | **Plates**: add, rename and duplicate plates; move objects between them; slice and print per plate or all in turn. Bambu 3MF plates map onto them | L |
| F-03 ✅ | Plates done 2026-11-15: `plates`/`plate` tools, the active plate scopes slice/check/print/arrange/render, Bambu 3MF plates import as plates and export to Bambu Studio as plates, plate tabs in the viewport (rename, duplicate, remove, move selection), eval `two_plates` passes | |
| F-11, F-04 ✅ | Workspace panel done 2026-11-15: PLATES (plates → objects with colour, size, hide, lock, filter, drag onto a plate, right-click), FOLDER (lazy tree of a chosen folder; open projects, import models, onto a new plate), PRINTS (history with the full trace). `workspace` tool, `locked`; evals `find_in_workspace` and `two_plates` pass. Parts done 2026-11-15: objects carry `parts` (own filament, move with the object, each checked on its own); multi-part Bambu 3MFs import with their parts (the orchids come in white with pink centres; modifiers no longer turn solid); renders, the viewport, 3MF export and the Bambu export keep the colours; MCP e2e + eval `two_colour_coaster` pass | |
| F-06 ✅ | Arrange done 2026-11-15: `arrange` ops align, dock, distribute/stack, grid, circle, swap, snap to cells, match_size (match/stretch/fill), copy_transform against the last-selected anchor or the plate; `duplicate_object` in a direction or a grid; Inspector Arrange section with 30 drawn buttons (⌥ = plate), each a bindable action (`arrange.*`); anchor outlined in the viewport; eval `arrange_pegs` passes | |
| F-05 ✅ | Gizmo done 2026-11-15: CAD triad by default (move arrows, rotate ring and box handles for width, depth and height on a selection; "One tool at a time" in Preferences), move snaps 1 mm (⌥ 0.1), rotation 15° (⌥ 1°), smart guides that snap to other objects' and the plate's edges and centres with gap labels (⌘ skips), values typed while dragging (Tab X/Y, Enter, Esc); all in Preferences / ui.json. Not yet: "rotate so this face points…" (lay flat exists) | |
| F-11 | **Workspace tree** like VS Code's explorer, on the left by default: projects and their plates, imported models, presets, exports and print history. Built together with plates, so plates live in it | M–L |
| F-04 | Object tree: plate → objects → **parts with their own filament** (so multi-colour Bambu objects keep their colours in the view, the export and the slice), with filament, visibility, lock, size and copies; search; right-click actions | M–L |
| F-05 | Gizmo: CAD triad + box handles, values typed while dragging, snapping (grid 1 / 0.1 mm, 15°, faces and edges), smart guides with distance labels, rotate so a face points a chosen way, lay on face | L |
| F-06 | Arrange, against the last-picked anchor (one object → the plate), all as shortcut-able tools: align, dock, distribute (equal or mm gap), stack, matrix, circular, duplicate in a direction, swap, match size, stretch, fill, smart resize, copy/paste transform, snap to plate-grid cells | L |
| F-13 ✅ | Sliders done 2026-11-15: thin track, round knob with a value bubble while dragging, a field to type the exact value, and a tick at the preset's own value (click it to go back) | |
| F-17 ✅ | Camera line done 2026-11-15: a camera toggle in the view strip draws a dashed vertical line at the position you enter in Preferences → Viewport (measured from the plate's front-left corner; not guessed) | |
| F-18 ✅ | Right-click menus done 2026-11-15: objects in the viewport (edit, hide, lock, lay flat, filament, arrange, move to plate, rename, settings, delete) and empty space (add here, import, paste, select all, arrange, add plate, view, slice); plate tabs, tree rows, parts, files, prints and activity rows have theirs; shortcuts shown | |

**Done when:** you lay out a real batch plate in only3d and print it, for example a set of
keychains or small tools, using plates and the arrange tools rather than typed coordinates.

**Status, 2026-11-15:** everything above is built and checked (tests, MCP end-to-end, screenshots;
the full agent eval suite passes 9/9, $2.33). What remains is the real-project print, which needs
you at the printer.

## Phase 2: functional parts, fast and right (about 2–3 weeks)

| Item | What | Size |
|---|---|---|
| C-2 / F-07 | Modelling core: fillet or chamfer on *chosen* edges, shell, 2D profiles with offset, a hole library (clearance, countersink, heat-set insert, teardrop for supportless printing), text, mirror, split, and the shapes picked from F-07 | L |
| C-4 | **Project checks**: checks saved with the project (dimensions, interference, clearance, overhang, wall thickness), run after every change and shown in the Checks panel. Your `check.sh` habit, built in | M |
| C-3 | **Fit calibration**: generate test coupons, record your caliper results per printer and filament, and feed the learned clearances into holes and parameters automatically | M |

**Done when:** a functional part (a hanger or a wall hook) is modelled, checked and
printed in only3d, and its fits come out right first time from your calibrated clearances.

**Status, 2026-11-15:** built and checked (unit tests, MCP end-to-end, screenshots, evals
`bracket_with_holes` and `calibrated_pin_hole` pass):
- **Shapes:** `slot`, `tube`, `polygon`, `wedge`, `text` (sans and mono, holes in letters kept),
  and `extrude` with `corner_radius`.
- **`shell`:** box, cylinder, slot, polygon and extrude; open or closed.
- **`hole`:** M2–M8 clearance, tap, countersink, counterbore, heat-set insert and nut trap, plus
  horizontal teardrops.
- **`split_object`** and `update_object {mirror}`.
- **Saved design checks** (`check {add}`): size, clearance, wall and overhang. They rerun on every
  change and show ✓/✗ in the Checks panel, which has an "add a check" form.
- **Fit calibration** (`fit`): a labelled coupon and pin; slip and press recorded per printer and
  filament; holes widen by the slip fit automatically. It's in the Spools panel too.

Not yet: fillets on chosen edges of arbitrary shapes (boxes and cylinders have them).
Remaining: a real functional part printed with your calibrated fit.

## Phase 3: print right the first time (about 2 weeks)

| Item | What | Size |
|---|---|---|
| C-5 | Trustworthy time and grams: include the H2D's start-sequence time and model acceleration properly; slicing quality items (overhang slowdown, elephant foot, monotonic surfaces, seams) | L |
| C-6 | Filament calibration stored with the filament: flow, pressure advance, max volumetric speed, tied to spools | M |
| F-08 | Spool registration panel: preset, colour from swatch or AMS, net weight, empty-spool weight, weigh, cards for each spool | M |
| F-19 | Spool location as a dropdown: the AMS slots, the external holders and every place you've used before, plus "New location…" for a custom one | S |
| C-1 | Finish the H2D link: phone notifications, AMS slot check after sending, one-click reprint | M |
| Debt | Verify on the H2D: an agent's approved print, AMS load/unload, answering a real alert | S |

**Done when:** only3d's time estimate is within about 5% of the real print on your next three
jobs, and a new filament can go from registered to calibrated to printing without leaving
only3d.

**Status, 2026-11-15:** built and checked (tests, MCP end-to-end, screenshots):
- **Spool panel:** material, preset, weights, location dropdown with "New location…", price; Move
  to and Calibration… on right-click.
- **Spool calibration:** flow, pressure advance and max volumetric speed, applied when slicing.
- **Learned print times:** the watcher records first layer, pauses and finish; slices say
  "on the printer ≈ …".
- **When a print ends:** a macOS notification, plus an optional ntfy URL for your phone.
- **Print again:** from Prints; it reopens the exact version printed, then the usual checklist.

Not built: overhang slowdown, elephant-foot compensation, monotonic surfaces and seams (slicing
quality), and an AMS slot check after sending.
Needs you: three real prints to train the times, and the H2D-verify list (an agent's approved
print, AMS load/unload, answering a real alert).

## Phase 4: batch, adapting models and onboarding (about 2 weeks)

| Item | What | Size |
|---|---|---|
| C-8 | Print queue and scheduled start, built on plates | M |
| C-7 | Bring in and adapt existing models: repair, split, hollow, add holes, hooks and text to STL/3MF | M–L |
| F-16 | First-run profile picker: only3d and Bambu Studio first, then Fusion, Blender, Onshape, Tinkercad, PrusaSlicer/Orca, and Advanced. Each sets keys, mouse navigation, gizmo and layout, with a summary before applying | M–L |

**Done when:** a new user picks "Bambu Studio" at first launch and feels at home. A MakerWorld
model gets adapted, for example scaled, hollowed with a hook added, then queued with other plates.

**Status, 2026-11-15:** built and checked (tests, MCP end-to-end, screenshots):
- **First-launch picker:** only3d, Bambu Studio, CAD, Blender or Tinkercad. It shows what it
  sets and saves the choice.
- **Print queue:** plates in order, a "next plate" notice when a print ends; you start each one.
- **Mesh repair** on import, and "Repair mesh" on right-click.
- **Adapting models** (holes, hooks, text, split) works through difference/union around the
  imported shape.

Not built:
- per-app mouse mappings (only the shortcuts and handle style change today);
- hollowing an arbitrary mesh (shell works on primitives);
- scheduled unattended starts. That one is deliberate: the H2D can't clear its own plate, and
  prints start only with you there.

## Feedback round after Phase 4 (built 2026-11-15)

From the user's test round:
- **Plates, Files and Prints are panels of their own.** Saved layouts gain Files and Prints once
  (`layout_version` in ui.json).
- **Arrange rows** are grouped by what they change: Align, Dock, Spread, Layout, Match,
  Stretch, Copy, Combine.
- **Duplicate moves nothing.** Copies go to the nearest free space beside the original. The
  Copies window (`arrange.copies`) does a count, a row, a grid (matrix) or fill the plate
  (`duplicate_object fill`).
- **Combine** (`combine` tool, right-click, Inspector row): assemble (each body keeps its
  filament), union, difference, intersection, and explode for an assembly. Everything stays
  where it was.
- **Spools on right-click:** the Filament menu lists the project's slots, then the spools on
  hand. Picking a spool adds or reuses its slot (`update_object spool`).
- **All plates at once:** the plates sit in a grid with the active one in focus and the others
  dimmed. Click a plate or its objects to activate it (the camera glides over). Each plate row
  has an eye (⌥-click shows only that plate), and a toggle sits beside the plate tabs.
- **Several filaments slice and print natively.** On the H2D each filament gets a nozzle (from
  its setting, else its spool's place, else spread over both). Layers are ordered for few
  changes, and changes use Bambu's own H2D template. Flushing in one nozzle follows a colour
  estimate calibrated to Bambu Studio 2.09. A prime tower takes each change (a simpler tower
  than Bambu's: no ribs or ironing). The idle nozzle cools while it waits and pre-heats in
  time. `print` maps each filament to its tray and checks that tray's nozzle. The preview can
  colour paths by filament. The start sequence and change blocks match Bambu Studio's output
  for the same two-colour flower plate, except for the order filaments start in.
  - **Tried on the H2D 2026-11-22 and it failed:** the change hung (F-20). Not usable for a real
    print until that is understood.
  - Not built: flushing into infill or supports, Bambu's rib-wall tower, painted colours.

## Second feedback round (built 2026-11-15)

- **Panes fold instead of closing.** Clicking the active tab of a split dock, or the chevron,
  folds that pane to its tab strip (a column of icons in the bottom dock). Panels closed in
  older layouts come back folded once (`layout_version` 3). "Hide" still takes a panel out;
  every pane's menu can add it back.
- **An empty plate is a starting point:** `empty_plate` is an info note, shown green as "No
  objects, ready to start". All objects hidden is a warning (`all_hidden`).
- **Drag models from the Files panel** onto a plate row, the Plates panel or a spot in the
  viewport.
- **Imports land clear of other objects** (`settle_import`): one block, nearest free spot, else
  a new plate, with a note. Nothing that was there moves; `add_object`'s last resort is a new
  plate too, not re-arranging the plate.
- **Spools name themselves** from brand + line + colour (Bambu's colour names for Bambu spools,
  the nearest of 40 common names otherwise) until the user types a name.
- **Design-app drag keys:** ⌥-drag copies, ⇧ locks X or Y, ⌘ moves with no snapping (it replaced
  ⌥ fine snap), and ⌘D repeats the last copy's step (`duplicate_object offset`). ⇧-drag on empty
  space still pans.
- **Push/Pull** (P): pull or push a flat face; a `push_pull` shape step, `update_object
  push_pull` for agents. Flat faces only; tapered walls and curved faces later.

## OpenSCAD and STEP (built 2026-11-16)

- **STEP import** (`.step`, `.stp`): monstertruck reads and tessellates the solids (0.05 mm
  chord), assemblies are placed, and inch or metre files are scaled to mm. One object per
  solid.
- **Live OpenSCAD variables:** a `.scad` import's customizer variables show in the Parameters
  panel (sliders with the file's ranges, drop-downs, checkboxes, text). Changing one re-renders
  with `-D`; the file is never edited. Stored as `vars` on the `import` node; numbers may be
  expressions over project parameters. `set_params {object}` for agents.
- **Export to OpenSCAD** (`export x.scad`, the `[scad]` button, File ▸ Export OpenSCAD…):
  project parameters as customizer variables, one module per object, small helper modules for
  only3d's primitives (rounded slabs, finished cylinders, slots, prisms, teardrops), `.scad`
  imports as `include` with their overrides. What OpenSCAD can't express (Push/Pull blocks,
  STEP/3MF meshes) goes in as a polyhedron or an STL beside the file. Renders to within 0.1 %
  of only3d's volume in the test.

## Friction from real prints (2026-11-20 to 2026-11-22)

Found while printing a five-plate project on the H2D with an agent driving only3d. IDs continue
from `docs/backlog.md` (F-19 is the last there); move them into its tables when its open edits
land. Sizes are rough guesses. "Seen" gives the print or code, so each can be reproduced.

**Do first:** F-20 and F-21 (a multi-filament print must not be offered as if it worked), then F-25
and F-29.

### A print went wrong

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-20 | bug | **A filament change partway through a print hung the H2D** | Seen: P-0023 (one change, A4 → A2, same right nozzle, at layer 5). It stayed in "Changing filament" for over 20 minutes with the new filament loaded, the nozzle steady at 210 °C and no HMS or print error. Pause and resume did nothing; it had to be stopped. The change block follows Bambu's H2D template, so the cause is unknown (firmware or our G-code). A load at the start of a print works (P-0028). Next: a tiny two-filament block with the user there, a diff against a two-filament plate sliced in Bambu Studio's window (its command line crashes on two filaments), then a fix | M |
| F-21 | bug | The preflight said a multi-filament print was fine | `print` returned `untested: null` and every checklist line ✓, although V5 in readiness.md says it was never run. Show "not yet verified on this printer" and require an extra yes, until F-20 is closed | S |
| F-22 | feature | No way to continue a stopped print | A cancelled job can't be resumed, so four good layers looked like scrap. Worked around by hand: the part was rebuilt as a body starting at z 0.68 with a 6 mm foot so the slicer skipped the finished layers, `bottom_layers` 0, levelling off. Proposal: "continue from layer N", which knows the finished height, keeps the part warm, skips levelling and calibrations that touch it, and checks every move against what is on the plate. P-0028 is that run; its result is not known yet | L |
| F-23 | feature | only3d can only send a slice it just made | A prepared `.gcode.3mf` can't be sent; the only ways to print one are a USB stick or Bambu Studio. Proposal: print a file (with the same preflight, checks and approval) | M |
| F-24 | feature | No control over what the start sequence does | `bed_leveling` and `timelapse` are the only switches. The levelling area is the whole footprint, so it can touch a part already on the plate, and the nozzle offset calibration still runs. Proposal: choose levelling area, nozzle offset, flow and first-layer checks per print | S–M |
| F-26 | ux | Running out of filament is not a planned step | Seen: P-0017 paused on `0700_8011`; the user fitted another spool in the same slot and resumed, and the toolhead clicked afterwards (the leftover end is the likely cause; the next test block was fine). Proposal: a guided swap (what is still in the tube, when to clear it, what to check after resuming), and a planned change at a chosen layer | M |

### Spools that can't be trusted

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-25 | bug/ux | **Grams left are treated as fact, and they aren't** | Seen: a spool listed at 91 g ran out after about 46 g; another went from 180 to 157 g with no print; a third read 4% (45 g), then 51 g after it was put back. The preflight then passes a job that needs 45.7 g from "51 g". Proposal: show an uncertainty (the AMS reports whole percent steps), warn when a job needs most of what is left, and ask to weigh | M |
| F-38 | bug | The default empty-spool weight is wrong for some spools | 250 g is assumed for all. One spool weighed 257.6 g while still holding about 41–51 g (by winding), so 250 g gives 8 g left. Take the tare from an emptied spool of the same kind, and show which figure was used | S |
| F-39 | bug | A spool that ran out still shows grams | The emptied spool kept 45 g after the run-out. Set it to 0 when the printer reports `0700_8011`, and log it | S |

### The printer link

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-27 | ux | The printer's address changed and only3d lost it | It moved to another subnet. It was found by scanning a port by hand, and the saved address had to be edited in `printers.json` because `printer_add` asks for the access code again. Proposal: find it by serial on every reachable subnet, and allow changing only the address | S |
| F-28 | bug | `printer_wait` returns at once while any alert is standing | It returned "after 0 s" whenever an HMS was present, so it could not watch a start. `wait_for_event` works. Wait for a change instead | S |
| F-29 | ux | **An approval expires after 5 minutes and nothing says so** | A stop request lapsed because nobody looked at the window, and the next one came back `denied_by_user`. Proposal: a macOS notification with the action, a longer or extendable wait, and the window's id in the result | S–M |
| F-30 | bug | The chamber camera is unreliable, and failures look alike | Seen: timeouts, `camera_busy` (another only3d process held it for 20 s), and a closed stream, next to the printer's own failures (`0C00_0300_0002_001D`, `0C00_0400_0002_0026`, `0C00_0100_0002_0008`, and `0C00_8034`, which pauses a print before its first layer). One shared camera reader per printer, and say whose fault it is. A preflight with no picture should say so. Branch `camera-grab` may cover part of this | M |
| F-31 | ux | Some printer codes have no text | `0500_0200_0003_000A` shows only "common (mainboard)". Show the code's own text or say it is unknown | S |
| F-32 | bug | A print was started but the call reported "Connection closed" | Seen 2026-11-21: the job was running, and repeating the call would have sent it twice. Make the start idempotent, or return the job on reconnect | S–M |

### Working with agents

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-33 | bug | Looking at G-code fills the print history | Every `export` of `.gcode` or `.gcode.3mf` adds an EXPORTED record: five in one afternoon (P-0022, P-0024 to P-0027). Don't record a plain export, or hide those records | S |
| F-34 | bug | An open window's project changed under an agent | Seen once: a session the agent had used now held an untitled project with an object it had not made. Cause unknown. The agent has to check the project's title before it writes | S |
| F-35 | ux | `printer_status` is long | About 8 KB of JSON each time, and agents poll it. Add a short form | S |
| F-36 | bug | Overhang and watertight warnings fire on bodies that touch | A part cut at a height and put back together reports 24,000–36,000 mm² of "overhang" at the joint, and `not_watertight`. Ignore faces that meet another body | M |
| F-37 | feature | No two filaments by height | `split_object` puts the halves side by side, so a two-filament part by height needed OpenSCAD to cut it and `parts` to put it back. Proposal: split in place, with a filament for each part (the workaround hung, F-20) | M |

## Running alongside every phase: agents

- Eval tasks grow with every feature, for example "lay out 20 keychains on two plates",
  "align these to the box and stack them" and "calibrate PLA fit and make a 3 mm pin hole". The
  pass rate, turns and cost are tracked per release.
- MCP progress notifications for long tools. Push notifications for resources only if clients
  use them.
- Agents can point at faces and edges (selection-aware), and versions can be compared side by
  side, never overwritten.

## Later, and not doing

- **Later:**
  - colour painting and flushing into infill or supports;
  - adaptive layer heights;
  - orientation chosen for function (where holes point, which way loads run);
  - scarf seams.
- **Not doing:** our own camera AI, AI image-to-3D, full assemblies and joints, laser and farm
  tools, exotic slicing, social feeds.
