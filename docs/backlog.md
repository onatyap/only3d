# only3d backlog

This backlog combines two sources:

1. The frictions you found using only3d (F-*).
2. The 80/20 capabilities from the 2026-11-15 benchmark (C-*). That benchmark covered:
   - slicers and printer apps: Bambu Studio, OrcaSlicer, PrusaSlicer, Cura, Handy, OctoPrint, bambuddy, Spoolman;
   - modelling tools: Fusion, Onshape, Shapr3D, Plasticity, OpenSCAD, build123d, Tinkercad;
   - AI and agent CAD tools;
   - your own print projects.

Priorities and phases are in docs/roadmap.md. ✅ marks items that are done.

**Type:** **bug** = something is broken · **ux** = it works but feels wrong ·
**feature** = something new.
**Size** is a rough guess: S under a day, M a few days, L a week or more.

## Your frictions

### Activity panel

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-01 | ux | ✅ Remove the "you + agents · live" header | A label only. Remove it; who did what stays visible in each row. | S |
| F-02 | feature | ✅ Rows should respond to hover and click (revised 2026-11-15: a click opens copyable text, right-click copies, selection only on request) | Hover highlights the row and the object it touched. Click selects that object, or opens the step's details (what changed, which revision); an "undo to here" could follow. | S–M |

### Objects panel

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-03 | feature | No plates: you cannot add plates | The project has a single plate today. Add plates as a first-class layer: add, rename, reorder and duplicate plates; move objects between them; slice and print a plate, or all plates in turn. Bambu 3MF plates already import and should map onto this. Ties to C-8 (batch and queue). | L |
| F-04 | ux | A more structured, thorough object panel | A tree: plate → objects → parts/modifiers. Each row shows filament colour, visibility, lock, size and copies, with search and filters. Right-click opens the actions (duplicate, arrange, split, set filament). Design together. | M |

### Inspector and selection

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-05 | feature | Better move/rotate/scale that feels native to 3D designers, with PowerPoint-like comfort. **Agreed:** the anchor is the last-picked object (one object → the plate); smart guides on by default; rotation snaps to 15° (configurable), plus "rotate so this face points…". Spec: docs/design/transforms.md | Two layers. (1) Viewport gizmos with snapping to grid, face and edge, numeric entry while dragging, and local/global axes, like Fusion, Shapr3D and Blender. (2) Panel buttons for exact values. Discuss: which tools you use daily. | L |
| F-06 | feature | Efficient Elements-style alignment and sizing. The full Efficient Elements → 3D mapping is in docs/design/transforms.md, waiting for your validation | Align and distribute against a **last-picked anchor**; match size and position; nudge by set steps; place on face, centre on plate, drop to plate. Review Efficient Elements' full set together. | M |
| F-07 | feature | More shapes | Today: box, cylinder, sphere, torus, extrude, revolve, import, booleans, hull, arrays. Candidates: rounded slot, tube, wedge or prism, polygon, cone/frustum, text, thread, screw hole and insert, plus 2D sketch → extrude with fillets on chosen edges. Overlaps with C-2. Discuss which come first. | M–L |

### First run

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-16 | feature | On first launch, pick the app you know best (only3d, Bambu Studio, Fusion, Blender…, or Advanced) and get its layout, keyboard, mouse navigation and gizmo style | The spec is in docs/design/transforms.md. The only3d and Bambu keymaps and the dock layouts already exist. Each profile is one keymap, one navigation map, one gizmo style and one layout. | M–L |

**Principle (agreed 2026-11-15):** everything is transparent and configurable. Defaults are
visible, every one can be changed, they are stored in readable files, and agents can read and
change them too.

### Spools

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-08 | ux | A better spool registration panel | Proposed flow: pick the material and brand from presets, the colour from a swatch or the AMS, net weight, and the empty-spool weight, then weigh to set what's left. RFID spools register themselves. Show every spool as a card with the amount left, where it is and when it was last used. | M |
| F-19 | ux | Spool location: a dropdown plus a custom/new entry | The AMS slots, external holders and places already used, plus "New location…". Phase 3, with F-08. | S |
| F-09 | bug/ux | ✅ `multi_filament_unsupported` error when slicing. Now "Open in Bambu Studio" | **By design**: the native slicer prints one filament per plate. The real problem is the dead end. Proposal: Slice falls back to Bambu Studio's slicer for multi-filament plates (`export_bambu_project`, which already works) and says so. Native multi-colour stays deferred; see "Not doing now". | S–M |

### Plate

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-10 | ux | ✅ Show which way the plate faces, like Bambu's text on the plate (thin, grid-coloured lines) | Print FRONT and BACK (and the H2D's nozzle zones, already drawn) on the plate in the viewport. | S |

### Projects

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-11 | feature | A workspace panel like VS Code's explorer, on the left by default | A folder tree of the project workspace: project files, imported STL/3MF/STEP, presets, exports and print history. Open, drag onto the plate, rename, reveal in Finder. Pairs with F-03 (plates) and the print history. | M–L |
| F-12 | bug | ✅ Importing a Bambu project while an object is on the plate made all the imported objects one colour | **Reproduced 2026-11-15** with `3_flowers_x4_white_pink.3mf`, and it happens on an empty plate too. Each flower is one object with white and pink parts; only3d keeps one filament per object, so the pink parts take the white. The import did say so, but the window never showed the note, and the note wrongly said `export_bambu_project` keeps the colours. Phase 0: an amber warning in the window and a true message. Real fix: parts with their own filament, in F-04. | S |

### UI

| ID | Type | Friction | What we know / proposal | Size |
|---|---|---|---|---|
| F-17 | feature | Show the built-in camera's location (a vertical line, toggleable) | Helps place objects where the camera sees them. Measure the H2D camera's position on the printer rather than guessing it. Phase 1. | S |
| F-18 | feature | Right-click menus on objects, plates and empty areas | Context menus on objects (viewport and tree), plates, parts, empty plate and viewport space. Entries are the existing tools, with shortcuts. The activity rows already have one (copy). Phase 1. | M |
| F-13 | ux | Better-looking sliders | Design together. Options: a thin track with a value bubble, a click-to-type field, and marks at preset values. | S–M |
| F-14 | ux | ✅ "Slice plate" is in an unintuitive place. Now one button, bottom right of the view | It appears in both the Print panel and the top bar. Proposal: one primary action at the bottom right of the viewport that follows the flow: Slice → Print… → Printing 42%. Discuss. | S |
| F-15 | bug | ✅ The Preferences UI-scale slider fights itself | **Cause found.** The scale is applied on every drag step, so the whole UI rescales under the pointer and the slider jumps to another value. Fix: apply on release, or use −/+ steps (⌘+ and ⌘− already work). | S |

## The 80/20 capabilities (benchmark)

| ID | Capability | Status today | Related frictions |
|---|---|---|---|
| C-1 | Cloud-free H2D link done right: send, monitor, camera, alerts, AMS mapping checked after sending, phone notifications, one-click reprint | Mostly done and verified on the H2D. Missing: notifications, the AMS check after sending, reprint | — |
| C-2 | Modelling core for functional parts: fillet/chamfer on chosen edges, shell, 2D profiles with offset, holes (clearance, countersink, insert, teardrop), text, mirror, split | Partial | F-05, F-06, F-07 |
| C-3 | Fit calibration from real prints: clearances per printer and filament, learned from test coupons and fed into holes and params. *No product does this.* | Missing | — |
| C-4 | Spec as tests: checks saved with the project (dimensions, interference, clearance, overhang), run after every change | Partial: the tools exist but checks are not saved per project | — |
| C-5 | Trustworthy time and grams, and slicing on a par with Bambu: overhang slowdown, elephant-foot compensation, monotonic surfaces, seams | Partial: start G-code and config identical to Bambu's; estimates −14…+40% | F-09 |
| C-6 | Filament calibration stored with the filament: flow, pressure advance, max volumetric speed | Missing | F-08 |
| C-7 | Bring in and adapt existing models: repair, cut/split, hollow, add holes, hooks and text to imported STL/3MF | Partial | F-12 |
| C-8 | Batch and queue: several plates, copies, scheduled start | Missing | F-03 |

**Foundation, done:** the agent layer.
- MCP, with approvals in the window.
- Events (`wait_for_event`) and resources.
- Prompts and the skill.
- The eval harness (5/5 tasks pass).

**Cheap, agent-native wins:**
- Explaining settings: Cura's #1 plugin is a settings guide.
- Choosing orientation for function (where the hole points, which way the load runs), not just overhang.

## Known gaps and debts from the build so far

- ~~Nothing is committed to git yet.~~ Committed and tagged `baseline` (2026-11-15).
- **Printer actions not yet tried on the H2D:**
  - an approved print started by an agent;
  - AMS load/unload with the corrected commands;
  - answering a real alert (resume/ignore/stop).
- **MCP extras missing:** push notifications for resource changes, and progress notifications during long tools.
- **Slicing quality backlog** (docs/upstream.md): overhang slowdown, elephant foot, look-ahead time estimates, monotonic surfaces, seams, gap fill, bridges.
- **bambuddy commands still to adopt:** AMS drying, pressure-advance management, the calibration bitmask, print options.
- ~~Leftover records~~: the six diagnostic records were deleted with `print_delete` (2026-11-15).

## Not doing now, and why

- **Native multi-colour slicing with flush minimisation.** Large; Bambu Studio handles these plates (F-09 makes that seamless).
- **Our own camera AI.** Users turn Bambu's off because of false stops; we relay the printer's own alerts instead.
- **AI image-to-3D.** MakerLab already does it, and the results are decorative, not functional.
- **Full assemblies and joints, laser and farm tools, exotic slicing, social feeds.**

## To discuss before prioritising

1. **F-05 / F-06:** which move/rotate/scale tools you use daily, and a walk through Efficient Elements' feature list together.
2. **F-07 / C-2:** which shapes and features come first.
3. **F-11:** what belongs in the workspace tree (just project files, or also presets, prints and spools).
4. **F-13 / F-14:** slider style and where the main action lives.
5. **F-12:** the Bambu project file you imported, to reproduce the bug.
