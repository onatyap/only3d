# Transforms, arranging and first-run profiles (draft spec)

Status: in discussion. This spec covers F-05, F-06 and the new F-16 in docs/backlog.md.

## Principles (agreed)

- **Transparent and configurable.** Every behaviour is visible where it happens: snap, guides,
  anchor and units show as chips in the status bar. Every default can be changed in Preferences
  and is stored in readable files (`~/.only3d/ui.json`, `keymap.json`). Agents can read and
  change the same settings through tools.
- **The anchor is the object you selected last**, drawn with a distinct outline. With a single
  object selected, actions work against the plate. A modifier key (configurable) forces the
  plate even with several objects selected, as Ctrl does in Efficient Elements.
- **Smart guides and distance labels** are on by default and can be configured.
- **Rotation snaps** to 15° by default (configurable), plus "rotate so this face points…".
- **Every arrange action can have a shortcut**, like Efficient Elements' "assign shortcut",
  through `keymap.json` and the command palette.
- **Size edits a parametric object's real dimensions**, so fillets stay true. Scale % is for
  meshes. In Magic Resizer, you choose what stays absolute.

## Efficient Elements → only3d (to validate)

The reference is the documentation for Efficient Elements 4.5 (Windows) and 4.123 (Mac), its
2020 handbook and the current feature page. The anchor is always the **last-selected** object;
with one object selected, EE uses the slide or drawing area. Rows marked *?* are icons in your
screenshot that the documentation does not confirm.

### Position

| # | Screenshot | Efficient Elements | only3d (3D) |
|---|---|---|---|
| P1 | Row 1, six orange icons | **Align** left / centre / right / top / middle / bottom to the anchor | Align X min/centre/max, Y back/middle/front, and Z bottom/middle/top (Z is new) |
| P2 | Row 2, four blue icons | **Dock** left / right / top / bottom: move until it touches the anchor | Dock ±X, ±Y, ±Z; Z gives "stack on top" or "put under". An optional gap, default 0 |
| ~~P3~~ | Row 2, "1x / 2x" icon | Golden Canon | **Dropped** (2026-11-15) |
| P4 | Row 2, teal Z icon ▾ | **Align in Matrix**: grid in selection order | Arrange in a grid (rows × columns, gap) in selection order. Good for batch plates (20 ghosts) |
| P5 | Row 3, icons 1–2 ▾ | **Stack** horizontal/vertical: touching, in selection order. *?* The dropdown may hold **Distribute** (outer objects fixed, equal gaps) | Stack along X/Y/Z in selection order. Distribute with equal gaps **or a set gap in mm** (EE has no value) |
| P6 | Row 3, diagonal-arrows icon ▾ | **Swap**: a dialog with X (left/centre/right), Y (top/middle/bottom) and Z-layer checkboxes | Swap positions per axis, with the reference edge; Z swaps heights |
| P7 | Row 3, table-with-crosshair icon | **Align in Table**: snap icons to the table cell beneath them | **Keep:** snap to the nearest cell of a configurable plate grid |
| P8 | Row 3, four-squares icon ▾ | **Slice or Multiply**: split or copy a shape into rows × columns with exact spacing | **Multiply**: copies in rows × columns with mm spacing. **Slice**: cut an object into pieces, e.g. for bed size |
| P9 | Row 4, icon 1 | **Arrange circular**, identified by you | **Keep:** arrange the selection evenly on a circle around the anchor or the plate centre (radius, start angle, facing outward or not) |
| P10 | Row 4, icon 2 ▾ | **Duplicate** right / left / up / down, identified by you | **Keep:** duplicate next to itself along ±X, ±Y or +Z, touching or with a gap. Repeats with the same step |
| P11 | Row 4, icon 3 ▾ | **Centre on corner**: put the object's centre on the anchor's top-left / top-right / bottom-left / bottom-right corner, identified by you | **Folded into Align**: a "my centre to its edge" option, plus snapping to the anchor's corners while dragging |
| P12 | Row 4, icon 4 | **Save position and size**, identified by you | **Keep as copy transform** (⌘⇧C): one clipboard slot, no named placements |
| P13 | Row 4, icon 5 ▾ | **Apply position and size**, identified by you | **Keep as paste transform** (⌘⇧V). Main use: put v2 of a part exactly where v1 was |

### Size

| # | Screenshot | Efficient Elements | only3d (3D) |
|---|---|---|---|
| S1 | Row 1 icon 1 ▾, row 1 icon 2, row 2 icon 1 ▾ | **Align Size**: same width, same width and height, same height as the anchor | Match size X / Y / Z / all; parametric objects change their real dimensions |
| S2 | Row 1, four green icons | **Stretch** left / right / top / bottom to the anchor's *far* edge | Stretch ±X, ±Y, ±Z to the anchor's far side |
| S3 | Row 2, four red icons | **Fill space**: extend to close the gap to the anchor | Extend until touching the anchor, e.g. a rib to a wall |
| S4 | Row 2, "A A" icon | **Magic Resizer**: percentage or target width/height, with "automatic scaling" of font size, line weight and margins | **Smart resize**: percentage or target size, with what stays absolute: fillet radii, wall thickness, hole sizes, text height. The 3D version of EE's auto-scaling |

### Also from Efficient Elements

- **Multi-select order matters**: Stack, Matrix and Swap follow the order of selection.
- **Select similar**: select every object with the same filament, shape type or size. *Keep?*
- **Hide/unhide temporarily**: only3d has hide already.

## Decisions (2026-11-15)

- **Dropped:** Golden Canon (P3).
- **Kept from row 4:** arrange circular (P9), duplicate in a direction (P10), copy and paste
  transform (P12 + P13). Centre on corner (P11) is folded into Align.
- **"Left/right/back/front" means the plate's axes,** whatever the camera angle.
- **only3d's default profile:** a CAD triad for move and rotate, box handles for size, values
  typed while dragging, and smart guides on.
- **Snap to the cells of a plate grid:** yes (P7). The grid size is configurable.
- **Proposed defaults, still open to change:**
  - Smart resize keeps fillet radii, hole sizes, wall thickness and text height fixed.
  - Distribute uses equal gaps; Alt+click asks for a mm gap.
  - Dock and duplicate use a gap of 0.
  - Rotation snaps to 15°; move snaps to 1 mm (0.1 mm with Alt).

## What "gizmo feel" means

A **gizmo** is the set of handles drawn on the selected object that you drag to move, rotate or
resize it. Tools differ in how those handles look and behave. That difference, together with the
mouse controls for orbit, pan and zoom, is most of what makes a 3D app feel native or foreign.

| Style | Used by | How it works |
|---|---|---|
| **Slicer** | Bambu Studio, Orca, PrusaSlicer, Cura | One tool at a time: M shows arrows, R shows rings, S shows scale boxes, plus a small panel of values. Simple and familiar to slicer users. |
| **CAD triad** | Fusion, Onshape, SolidWorks | One combined widget: arrows to move, arcs to rotate, small squares to move in a plane. A value box follows the cursor, and the triad can align to a face. |
| **Keyboard first** | Blender | G/R/S, then X/Y/Z to lock an axis, then type a number. Handles are optional. Very fast once learned. |
| **Direct, bounding box** | PowerPoint, Tinkercad, Shapr3D | Drag the body to move it on the plate. Corner and side handles resize, and a handle above rotates. Shapr3D lets you push and pull faces. |

**Mouse navigation also differs** (to verify per app):
- Bambu Studio: left-drag orbits, right-drag pans.
- Fusion: Shift+middle orbits, middle pans.
- Blender: middle orbits, Shift+middle pans.
- Onshape: right-drag orbits, middle pans.
- SolidWorks: middle orbits, Ctrl+middle pans.
- Tinkercad: right-drag orbits.

**Proposed only3d default:**
- a CAD triad for move and rotate;
- bounding-box handles for size, in the PowerPoint style;
- Blender-style typed values while dragging;
- smart guides.

The other styles come from the profiles below.

## F-16: first-run profile picker

On first launch you pick the app you know best. only3d then sets up keyboard, mouse
navigation, gizmo style and panel layout to match.

- **The choices:**
  - only3d (default);
  - slicers: Bambu Studio / OrcaSlicer, PrusaSlicer, Cura;
  - CAD tools: Fusion, Onshape, SolidWorks, Shapr3D, FreeCAD;
  - Blender and Tinkercad;
  - **Advanced**, where you choose each part yourself (keymap, mouse navigation, gizmo style,
    layout, units and snapping).
- **Before applying,** a summary lists exactly what the choice sets, in keeping with the
  transparency principle.
- **After that,** you can change the choice or any single part in Preferences at any time.
- **Built on what exists:** the keymaps (only3d, Bambu) and the dock layouts already work
  this way. Each new profile is one keymap, one navigation map, one gizmo style and one layout.
- **To verify for each app:** the actual default shortcuts and mouse mappings, taken from each
  app's own documentation, not from memory.
