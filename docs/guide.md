# only3d guide

The full reference; start with the [README](../README.md). only3d is a fast, structured, agent-first 3D modelling, slicing and printing studio in Rust, built
around the Bambu Lab H2D.

* **One API for people and agents.** Every action is a named tool with a JSON Schema
  (`only3d tools`). The GUI, the CLI, the MCP server and the live socket all call the
  same tools, so an agent can do anything a person can, with undo and an activity journal.
* **Parametric and plain-text.** Projects are diffable JSON (`*.only3d.json`). Geometry is a
  shape tree (box / cylinder / sphere / torus / extrude / revolve / import + union /
  difference / intersection / hull + array / polar). Numbers can be expressions over named
  params (`"wall*2 + 0.4"`), so "make the walls thicker" is one call.
* **Native and fast.** Pure-Rust geometry: a Manifold-class boolean kernel (`boolmesh`), the
  `i_overlay` polygon engine, parallel layer processing with `rayon`, and a headless renderer.
  Identical copies on a plate are sliced once. Measured on an M-series Mac against Bambu
  Studio 02.09's CLI, with the same presets:

  | job | only3d | Bambu Studio CLI | filament (ours / Bambu) |
  |---|---|---|---|
  | 20 mm cube, STL → .gcode.3mf | 0.12 s end to end (8 ms slicing) | 0.41 s | 3.76 g / 3.75 g |
  | 276 mm OpenSCAD hanger, 530 layers | 2.7 s wall, 5.8 s CPU | 3.2 s wall, 16 s CPU | 303.0 g / 306.4 g |
  | 20 keychains, 25 M triangles | 11 s wall (4.5 s slicing) | 6.6 s wall | 125.0 g / 123.7 g |
* **The genuine H2D start sequence.** The slicer renders Bambu Studio's own machine templates
  (start, end, layer change, timelapse) from the installed profiles, with `include` resolved.
  Flattening presets naively drops that and silently falls back to a generic start G-code.
* **Agents can see and verify.**
  * `render` returns PNGs of the model or the toolpaths.
  * `section` reports islands, holes and too-thin material at any height.
  * `clearance` measures fit gaps between parts, such as a pin in a socket.
  * `assembly` shows the printed parts fitted together with bought hardware that is never
    printed (a parachute, a motor) and reports where bodies overlap;
    `render {"assembly": true, "cut": {"axis": "y"}}` draws it as a cutaway.
  * `check` lists printability issues with stable codes and hints.
  * `batch` runs several edits as one all-or-nothing step.
  * `printer_wait` follows a print to a layer or to completion.

## Crates

| crate | what it does |
|---|---|
| `only3d-geom` | Meshes, analysis (watertight, volume, overhang, bed contact), primitives with fillets and chamfers, convex hull, STL/OBJ/3MF/STEP IO, software renderer |
| `only3d-model` | Project document, parametric shape trees with expressions, memoised evaluation, CSG, arrange, auto-orient, printability checks |
| `only3d-slice` | Native slicer: layer slicing, walls, top/bottom/solid/sparse infill, bridges, brim, cooling, Bambu-style G-code with M73 progress, motion-aware time estimate, `.gcode.3mf` packaging |
| `only3d-profiles` | Bambu Studio preset store (inherits + include), H2D variant arrays, the placeholder/template engine for Bambu's G-code macros |
| `only3d-bambu` | LAN printer link: SSDP discovery, MQTT over TLS with certificate pinning, typed H2D status (both nozzles, AMS), FTPS upload with TLS session reuse, `project_file` print start |
| `only3d-studio` | Session, tool registry (the API), undo/redo, journal, describe/render/section, machine setup, printer tools, live socket |
| `only3d-gui` | egui/wgpu desktop app: viewport, params, inspector, print panel, activity feed |
| `only3d` | The binary: `gui`, `mcp`, `call`, `tools`, `slice`, `render`, `info`, `serve` |

**The desktop app**

* **Look:** Dark, Light, Night or System (follows macOS), with mono section labels, a
  dot-grid viewport and a pixel logo.
* **Docks:** panels are tabs in left, right and bottom docks. Drag them, split them, collapse
  them (⌘B, ⌥⌘B, ⌘J; ⇧⌘⏎ for zen mode). The layout is remembered in `~/.only3d/ui.json`.
* **Menu bar (macOS):** File, Edit, Object, Arrange, Plate, View, Printer, Window and Help,
  with the shortcuts of your keymap. Help's search box finds any menu command.
* **Command palette:** ⌘K or ⇧⌘P reaches every action and every agent tool.
* **Tabs:** files open as tabs beside the 3D plate, like a code editor. A click in Files opens a
  preview tab (italic) that the next click replaces; double-click keeps it. Drag tabs to reorder
  them or to the right edge to split; ⌘\\ splits and joins, ⌘W closes, ⇧⌘T reopens, ⌃Tab
  cycles. Files dropped from Finder open as tabs; models dropped on the plate import.
* **Save (⌘S):** a name and a folder. The title bar says "Edited" while there are unsaved
  changes, and closing the window, quitting (⌘Q), New or opening another project then asks
  Save / Don't Save / Cancel.
  **Export (⇧⌘E):** tabs by purpose (For printing, 3D
  model, Picture, Project), each with its formats and a recommended one; a preview of what goes
  out; the selection, this plate or all plates; one file or one per object (per plate); STL binary
  or text, picture size and view; "Show in Finder afterwards". It remembers the last format.
* **Keymaps:** "only3d" (macOS style) or "Bambu Studio" (Bambu's own bindings), chosen
  in Preferences (⌘,). Overrides go in `~/.only3d/keymap.json`; press `?` for the sheet.
* **Direct manipulation:** drag objects on the plate. M/R/S switch between move, rotate and
  scale; arrows nudge; `[` and `]` rotate 15°.
* **Panels:** a searchable Settings panel; preset pickers for your Bambu Studio presets with
  save-as, import and export; plus Spools and Printer panels.

## Quick start

```bash
cargo build --release
./target/release/only3d                                     # the desktop app (agents attach live)
./target/release/only3d gui part.stl                        # open a model or a project
./target/release/only3d tools --brief                      # the whole API
./target/release/only3d slice part.stl --set infill_density=0.25 -o part.gcode.3mf
./target/release/only3d render project.only3d.json --view iso -o view.png
./target/release/only3d call add_object '{"shape":{"type":"box","size":[40,20,10],"radius":2}}' -p demo.only3d.json
```

### Settings and presets

* **Settings catalog:** `settings_catalog` lists all 81 slicer settings in 11 groups. The
  groups are quality, walls, infill, support, adhesion (brim and skirt), speed, acceleration,
  temperature, cooling, retraction and filament. Each setting has a label, unit, range, an
  explanation, its effective value and any override. `per_object` marks the 40 an object can
  override (`update_object` settings: walls, infill, surfaces, speeds, brim, supports); the
  rest belong to the whole plate. Object overrides are checked when they're saved, not when
  slicing: an unknown key (Bambu's `brim_type` says to use `brim_width`), a plate-wide key or a
  bad value is refused.
* **Supports:** `support` turns them on under overhangs flatter than `support_angle`.
  `support_type` follows the process preset (`tree` in Bambu's): branches from tips under a
  roof of interface lines lean together, merge, thicken and stand on flared feet, keeping
  out of the part and resting on it (on a floor, below a Z gap) only where they can't reach the
  plate. `normal` gives straight columns. `support_on_plate_only` drops what can't reach.
  `support_interface_filament` prints the roof in another filament slot (a support material,
  or PETG under PLA): it then touches the part with solid lines and no Z gap, on the H2D's
  other nozzle by default, primed on the tower; floors on the part stay in its own filament.
  `tools/supports_verify.py` compares them with Bambu Studio's on a few test parts.
* **Findings and what covers them:** a tall part on a small footprint (`tip_over_risk`) or an
  overhang (`needs_support`) stays in `check`, but as Info with a `mitigation` when the slice
  already prints a brim or supports for it.
* **Presets are data, in Bambu Studio's own JSON format:**
  * `preset_list` shows your Bambu Studio user presets first, then only3d's (in
    `~/.only3d/presets`), then the compatible system presets.
  * `preset_get` reads a preset.
  * `preset_create` builds one from a base preset, using Bambu keys or only3d keys. With
    `from_project` it captures the current project's overrides.
  * `preset_update` and `preset_delete` work on only3d presets only; Bambu Studio's are
    never modified.
  * `preset_import` accepts `.json`, `.bbscfg`/`.bbsflmt`/`.zip` bundles, or a `.3mf` project,
    whose changed settings become presets.
  * `preset_export` writes Bambu JSON you can import into Bambu Studio.

### Colours from 3MF projects

`import_file` brings in each object's filament slot plus the file's slot colours and presets,
read from Bambu Studio and Orca project metadata or 3MF materials. Multi-plate projects import
one plate at a time (`plate`). A multi-colour object keeps its largest part's filament; print
those through `export_bambu_project`.

### Filament inventory and print traceability

Everything lives in an embedded SQLite database, `~/.only3d/only3d.db`.

* **Ledger:** a spool's remaining weight is the sum of its ledger (`spool_events`). Each gram
  change records its source (`add`, `print`, `ams`, `weigh`, `manual`) and, when a print caused
  it, the print ID.
* **Prints link back to designs:** every print (`P-0001`…) points at a content-addressed
  snapshot of the exact project version. It lists each object with its estimated grams and Bambu
  object ID, and which spool and tray fed each filament slot. So
  `spool → ledger → print → project version → objects` is answerable at any time.
* **Sources of truth:**
  * Bambu RFID spools are charged by what the AMS measures (`remain_g`), and drops during a
    print are attributed to that print.
  * Spools without RFID are charged the slicer's estimate, then corrected with `spool_weigh`.
    Nothing is counted twice.
  * A used RFID spool the AMS hasn't measured yet is registered as full and flagged until the
    AMS measures it (that first measurement isn't charged to a print) or it is weighed.
  * A spool whose AMS slot reports empty goes to the shelf. An untagged spool in a slot where an
    RFID spool is filed is flagged, never taken for it.
  * A Bambu spool registered before it was loaded (brand "Bambu Lab") takes its RFID tag the
    first time the AMS reads it: the unloaded, untagged spool of the same material and line with
    the nearest colour. Its first AMS reading replaces the registered weight and isn't charged to
    a print.
  * `printer_ams {"action": "read_rfid", "slot": 0}` has the AMS read a slot's tag again, like
    the refresh button in Bambu Studio. It turns the spool and measures what's left (about 15 s).
    It only works on a slot with filament at its inlet. A spool sitting on the rollers isn't
    seen until its filament end is pushed in, and with "Insertion update" on the AMS then reads
    it by itself.
* **Registering many spools:** `spool_add` takes `quantity` for identical spools (each gets its
  own ID) and `gross_g` for a scale reading of a used spool. Once a brand and line have been
  registered, their net weight, empty-spool weight, price and preset are filled in for the next
  one. An AMS slot or external holder holds one spool; a place like "dry box" holds any number.
* **Prints from Bambu Studio count too:** exporting a `.gcode.3mf` registers it. When that file
  starts printing from Bambu Studio or Handy, `spool_sync` (or the 2-minute tracker in the app
  and in `serve`) links the job back to its project version. Jobs from elsewhere are recorded
  as well.
* **Tools:**
  * Spools: `spool_add`, `spool_list`, `spool_detail` (the ledger), `spool_update`,
    `spool_weigh`, `spool_use`, `spool_sync`.
  * Prints and reports: `print_history`, `print_detail` (the full trace), `print_update`
    (outcomes, per-object failed/skipped), `usage_report` (by spool, material, project or month),
    `project_versions` (inspect or restore a printed design).
  * Queries: `inventory_query` for read-only SQL.

### Use it from agents (MCP)

only3d works with any agent app that speaks MCP, whatever model runs inside it. In the window,
only3d ▸ Connect an Agent… does this per app; from a shell:

```bash
only3d mcp install                    # the apps, whether only3d is in each, and the server entry
only3d mcp install codex gemini       # add it (several at once is fine)
only3d mcp install opencode --print   # just show what to paste
```

The apps are `claude-code` (`claude mcp add --scope user`), `claude-desktop`, `codex` (the
Codex app and CLI), `gemini` (Gemini CLI), `antigravity`, `opencode`, `kimi` (Kimi Code) and
`cursor`. Config files are merged, not replaced: other servers stay, the old file is kept as
`.bak`, and a file with comments is left alone (you get the entry to paste instead). Models
without an app of their own, such as DeepSeek, run inside one of these: opencode takes any
provider, and Claude Code can use DeepSeek's and Kimi's Anthropic-compatible APIs.

`only3d mcp install skill` writes `~/.agents/skills/only3d/SKILL.md` for agents that read Agent
Skills and work through a shell instead of MCP: it teaches them `only3d call <tool> '<json>'`,
which works on the open window (`--session <id>` picks one when several are open).

This repository also ships a `.mcp.json`, so Claude Code sessions started here offer the
server automatically. It expects `only3d` on PATH, for example via `cargo install --path crates/only3d`.
To keep one project file in sync, add `--project ~/prints/current.only3d.json --autosave` to
`only3d mcp`.

The server speaks MCP 2025-06-18 over stdio and covers:
- every tool, with a JSON Schema (about 12k tokens for all 68);
- read-only, destructive and idempotent hints;
- structured results, and images for renders and camera snapshots;
- errors as tool results with a stable `code` and a `hint`.

`crates/only3d/tests/mcp.rs` drives it end to end the way an agent does.

**Agents see what happens.**
- `wait_for_event` returns, or waits for, the next events:
  - your edits and selection, labelled with who made them;
  - finished slices;
  - approval decisions;
  - the printer: state changes, stages (heating, leveling, purifying the air), alerts, layer progress, finish or failure, connection loss;
  - the AMS: drying starting, finishing, stopped early or failing (`ams_drying`), and spools inserted, removed, read or set to another filament (`ams_slot`).

  An agent can react instead of polling. Over the live session it waits without locking anything, and `printer_wait` now does too.
- MCP resources expose the open project, your selection, printer status, a fresh camera photo, recent prints and recent events, to attach as context.

**Ready-made workflows.**
- The server offers MCP prompts for the main journeys: `design_part`, `print_plate`, `diagnose_print` and `watch_print`. Claude Code lists them as `/mcp__only3d__<name>`.
- `.claude/skills/only3d/SKILL.md` teaches any Claude Code session working in this repo how to use only3d well:
  - the working loop: look, parameters, verify, slice;
  - the approval rules;
  - waiting for events;
  - handling alerts;
  - the H2D's quirks.

  Copy it to `~/.claude/skills/` to have it everywhere.

**Measuring how well agents do.**
- `python3 evals/run.py [task...]` runs each task in `evals/tasks/` with a real `claude -p` agent. Each run uses a throwaway headless session with no printer and only only3d's non-printing tools.
- It then checks the resulting model automatically: sizes, parameters, a clean printability check, a current slice, the answer.
- It reports pass/fail, turns, tool calls, tool errors, cost and time, and writes the details to `evals/results/`. Use it to judge changes to the tools by whether agents get better.

**Agents can handle printer alerts.**
- `printer_alert` lists the current alerts with Bambu's text and the actions Bambu Studio offers for each. The action table is read from your Bambu Studio installation, per printer model.
- With your approval it performs one of those actions: resume, stop, ignore and resume, don't remind, OK, or AMS done/retry/abort. The commands match what Bambu Studio sends.
- `printer_nozzle` switches the active nozzle.
- `printer_ams` load and unload use Bambu Studio's addressing for AMS slots, AMS-HT units and both external holders. These payloads were checked against bambuddy.

**Drying filament.** `printer_ams` action `dry` dries the filament in an AMS 2 Pro or AMS HT,
and `stop_drying` stops it. They send the same command as Bambu Studio's drying dialog.
- Without `confirm`, `dry` shows the plan: the filament, temperature and hours. They default to
  what Bambu's filament preset recommends for that AMS model, for the loaded filament that
  needs the lowest temperature. Pass `filament_type`, `dry_temp` or `dry_hours` to change them,
  and `rotate_spools` to turn the spools while drying.
- It keeps Bambu Studio's limits: 45–65 °C on the AMS 2 Pro, 45–85 °C on the AMS HT, 1–72 h.
  During a print the temperature is capped at what the loaded filaments tolerate then.
- It won't start when another loaded filament's end would soften at that temperature and
  could clog its inlet. Dry cooler, or pull those ends out of the inlets first.
- It passes on the printer's own reasons it can't dry now, such as filament in the AMS outlet.
- In the Printer panel, each AMS that dries has a "dry…" button (the first run shows the plan;
  set `confirm` to true to start) and "stop drying" while it runs.

**You approve what agents do to the printer.** Some agent requests start, stop or destroy
something:

- starting a print;
- stopping one;
- answering an alert;
- switching nozzles;
- skipping objects;
- deleting printer files;
- loading or unloading filament;
- heating the nozzle or bed, even on an idle printer (turning a heater off needs no approval);
- drying filament in an AMS (stopping it needs no approval);
- heat or fan changes during a print.

only3d asks you before any of them runs. The window shows a dialog with the request, the
preflight checklist and a photo of the plate, and the agent's call waits for your answer.
With no window open, MCP clients that can ask their user (elicitation) ask there. Otherwise the
request is refused. An agent setting `confirm: true` only *asks*. A desktop notification
(macOS and Linux) announces each request, so you see it away from the window. Requests are
denied after 15 minutes without an answer, with another notification saying so, and every
decision appears in Activity.

### Which window an agent works in

Every only3d window (and `only3d serve`) is a live session with a short id, like `k7m2`, shown
on its agent link chip at the bottom right, along with the agents working in it. Sessions list
themselves in `~/.only3d/sessions`, whatever `ONLY3D_HOME` a window runs with; `only3d sessions`
shows them. An agent never picks one by luck:

- One session open: the agent works there. Its first result names the window, so it can tell you.
- Several open: the call fails with `session_choice_needed`, listing them. The agent asks you
  which, then calls the `session` tool with that id (`only3d call --session <id>` from a shell).
- None open: the MCP server opens a window and works there. Where it can't (no display), the call
  fails with `no_window` and the agent asks you to open only3d.
- The window closes mid-task: the call fails with `window_closed`. Nothing carries on elsewhere.

Work without a window happens only when asked for: `only3d mcp --no-live`, `--project <file>`,
or the `session` tool with `headless: true`. The agent is then told you can't see its work.

The agent's edits appear on screen as they happen, and the agent can ask what you selected
(`get_selection`): "make *this* 2 mm taller" just works.

### Printing on the H2D

Tested against a real H2D (firmware with Bambu Lab's 2025 authorization system, cloud mode)
on 15 Nov 2026:

| Feature | Result |
|---|---|
| Live status over MQTT/TLS | works: both nozzles, bed, chamber, AMS trays, external spool, last job |
| Certificate pinning | works. The H2D's certificate is an X.509 version that webpki rejects, so only3d verifies handshake signatures from the raw key itself |
| FTPS login and listing | works, including the TLS session reuse vsftpd demands |
| System commands in cloud mode | work: the chamber lights (both H2D nodes) switched off and back on, confirmed by the printer's own report |
| Print commands in cloud mode | rejected by the firmware ("mqtt message verify failed", code 84033543): speed profile, G-code temperatures and fans, and so presumably pause/stop and print start. only3d reports this as `printer_auth_required` with the fix: LAN-only mode plus Developer Mode |
| Filament tracking | works: AMS weight drops during a real print were attributed to its print record (P-0001: 4 g of PETG Translucent) |
| Uploads | need removable storage: a USB flash drive in the H2D's top port (microSD on X1/P1/A1). Without one the FTP root is empty and cloud jobs go to internal storage instead |
| Starting prints from third-party software | needs LAN-only mode with Developer Mode on (confirmed indirectly: other print commands are rejected in cloud mode). Without that, use `export_bambu_project` and print from Bambu Studio |

Printer tools:

* **Read:** `printer_status`, `printer_raw` (everything the printer reports), `printer_files`,
  `printer_camera` (JPEG snapshot), `printer_wait`.
* **Write:** `printer_set` (speed profile, light, bed and nozzle temperatures, fans),
  `printer_ams` (slot info, load and unload), `printer_file` (download, delete),
  `printer_control` (pause, resume, stop), `printer_skip_objects`, and `print`.
* **Timelapses:** tick "Record a timelapse" in Send to printer, or pass `timelapse: true` to
  `print`. The printer takes a picture each layer, where the nozzle is (no parking the head
  for each), and keeps the video on its USB drive or card; videos on internal storage can't be
  listed over the LAN. The Printer panel's Timelapses section, or `printer_files` with
  `{"timelapses": 3}` (the newest three thumbnails), lists them newest first with the print that was
  running then (matched by time, so the printer's clock and time zone should be right).
  `printer_file` downloads or deletes one; deleting leaves its thumbnail on the printer.
  Listing was checked on an H2D with no videos on it; thumbnails and downloads only against a
  test server.
* **Gates:** anything that would disturb a running print needs `confirm: true` or an idle
  printer.
* **Camera:** on the H2D, turn on Settings → Network → "LAN Only Liveview". The printer reports
  `ipcam.rtsp_url = "disable"` until then.

Setup:

1. On the printer, enable LAN mode and note the 8-digit access code, the IP and the serial
   number.
2. Run `only3d call printer_add '{"name":"h2d","host":"192.168.x.x","serial":"…","access_code":"…"}'`.
   The config is stored in `~/.only3d/printers.json` with mode 600. The TLS certificate is
   pinned on first connect.
3. Use `printer_status` to see temperatures for both nozzles, the AMS trays and progress. Each
   AMS unit also reports its model, humidity, temperature and drying (filament, temperature,
   time left, or why it can't dry now), and what the AMS is doing (reading RFID, changing
   filament). The Printer panel shows the same in its AMS section, and the app tells you
   when drying finishes or fails.
4. `print` runs a preflight checklist: genuine machine G-code, printability checks, printer
   idle with no fatal or serious HMS error, storage present, and each filament's tray. It lists
   every tray as the printer's screen names it (A1..D4, ext left/right) with the spool's name,
   colour and what's left, and checks there is enough filament. A tray of another product line
   or colour than the project's is your call: the preflight asks which tray to use instead of
   picking one. In the window, Send to printer shows a tray picker for each filament; one whose
   loaded filament isn't the project's starts empty, and Start print stays off until you pick.
   It only uploads and starts with `confirm: true`. Agents are told to confirm
   only when you asked them to.
   A plate with several filaments isn't verified on any printer yet: on the H2D a filament
   change partway through a print hung it once. The preflight says so, and the print needs an
   extra yes: the "Start it anyway" tick in Send to printer, or `multi_filament_ok: true` from
   an agent you told. When a printer's multi-filament print is verified, its entry in
   `profiles/printers.json` says `"multi_filament": {"status": "tested"}` and the extra yes goes
   away.
   A plate isn't started twice: while the printer prints a job of its name, or for a few minutes
   after it was sent, `print` refuses with `already_started` (Send to printer says so too). An
   agent whose connection dropped while it waited for your answer is told the print may have
   started, instead of that nothing was done.

## Shape cheat sheet

```json
{"type":"difference","children":[
  {"type":"box","size":[60,30,"t"],"radius":2},
  {"type":"array","count":[2,1,1],"spacing":[40,0,0],"at":[-20,0,-1],
   "child":{"type":"cylinder","d":5.4,"h":20,"chamfer_top":0.5}},
  {"type":"cylinder","d":12,"h":20,"at":[0,0,3]}]}
```

* All sizes are in millimetres, with Z up. Primitives are centred on X/Y and rest on Z=0;
  `"center": true` also centres them on Z.
* Node transforms apply in this order: `scale` → `rotate` [x,y,z]° → `mirror` → `at`.
* `box` takes `radius` (rounds every edge), `chamfer` (45° bevel on every edge), or
  `corner_radius` (rounds only the vertical edges, keeping the top and bottom flat for trays,
  lids and plates) plus `chamfer_top`/`chamfer_bottom`/`fillet_top`/`fillet_bottom`.
* `cylinder` takes `d`/`r`, `h`, `d2` (for a cone), and `chamfer_top`/`chamfer_bottom`/
  `fillet_top`/`fillet_bottom`.
* `hull` gives soft blends; `import` takes stl, obj, 3mf, step, or scad (scad needs OpenSCAD installed;
  its customizer variables are set with `set_params {object}`). `export` also writes `.scad`.

## Status

* **Verified:** the start, layer-change, timelapse and end G-code is byte-identical to Bambu
  Studio's own output for the same job (snapshots in `crates/only3d-profiles/tests`). Every slice
  is also checked by an independent G-code linter (reach of the chosen nozzle, feed rates, Z
  order, extrusion per mm) before it can be sent.
* **Not yet native:** multi-filament plates (one filament per plate), a prime tower, arc
  fitting, and gap fill. For these, `export_bambu_project`
  writes a Bambu Studio project with the same presets, colours, positions and overrides.
  It is sliced by Bambu's CLI when there is a single filament; open it in Bambu Studio to
  finish multi-filament plates.
* **Estimates:** filament use matches Bambu within about 1%. Print time runs from 14% lower
  (small parts) to 40% higher (dense organic meshes) than Bambu's estimate.
* **Hardware:** live status, FTPS and filament tracking are tested on a real H2D (see
  Printing on the H2D). A native print start needs LAN-only mode with Developer Mode and has
  not been run on hardware yet: make the first one a small test part, and watch it.

License: AGPL-3.0-or-later (the embedded Bambu Studio profiles are AGPL-3.0).
