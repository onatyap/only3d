# Changelog

## Unreleased

Fixes for friction found while printing (roadmap F-27 to F-43).

- The Windows programs carry only3d's icon and version details, and the installer says what it changes and what only3d sends over the network ([docs/privacy.md](docs/privacy.md)). Releases can be signed through SignPath Foundation ([docs/code-signing.md](docs/code-signing.md)).
- **A run-out empties the spool:** when the printer reports that filament ran out
  (`07xx_8011`), the spool in the active nozzle's tray goes to 0 g in the ledger, once per
  run-out, so a fresh spool put in the same slot during that pause keeps its grams.
- **Empty-spool weights are measured, not assumed:** weighing a spool that ran out records its
  real empty weight (one Bambu spool weighed 210 g, not the assumed 250 g), and new spools of
  that brand and line start from it. `spool_weigh` says which empty weight it used and whether
  it was measured or assumed.
- **A printer that moved is found again:** when the saved address doesn't answer, only3d looks
  for the printer by its serial and by its pinned certificate on this computer's network and
  around the old address, then saves where it is now. `printer_add` with just a name and a new
  `host` changes only the address; it keeps the access code and the pinned certificate.
- **A stuck stage is reported:** a stage of a running print that runs far past its usual time
  with nothing moving (a filament change over 4 minutes) logs `printer_stuck`, sends a
  notification from the window, and ends a `printer_wait`.
- **Stopping a print can keep the bed hot:** `printer_control` stop takes `hold_bed`, so the
  part stays put to be continued. Without it, a stop that leaves a part on the plate says the
  bed is cooling and how to set it back.
- **`printer_wait` waits:** an alert that was already standing when the wait began no longer
  ends it after 0 s; only a new alert or print error does.
- **Alert codes always say something:** a code whose entry in Bambu's list is empty, or that
  isn't in it, says so instead of showing nothing. Print errors (such as `0700_8011`, filament
  ran out) now show their text in the status, the event log and the preflight too.
- **A short printer status:** `printer_status {brief: true}` returns a few lines and a small
  object (state, progress, temperatures, the active nozzle's tray, alerts) instead of ~8 KB.
- **Exports stay out of the print history:** exporting G-code no longer adds a print to the
  history and the Prints panel (it is still recorded, to trace it if it's printed from Bambu
  Studio or Handy), and exporting the same file again replaces its earlier record.

## 0.2.0 (2026-11-26)

- **Approval requests you can't miss:** a stop request lapsed after 5 minutes because nobody
  was looking at the window. A desktop notification now announces each request, and another
  says when one lapses (macOS and Linux). Requests wait 15 minutes, and the agent is told the
  request lapsed, that nothing was done and which window it waited in. When a lapsed request
  gives way to the next, the dialog's buttons wait a moment, so a click meant for the old one
  doesn't answer the new one.
- **A print isn't started twice:** an agent's print call reported "Connection closed" while
  the print was running, and repeating it would have sent it again. `print` now refuses a plate
  the printer is printing, or one this window sent minutes ago that the printer may not show
  yet, with `already_started`. When an agent's connection to the window drops during a print
  or another approved action, it's told the action may have been done and to check first,
  instead of that nothing was done.
- **Long waits through the window:** `printer_wait` and `wait_for_event` called through an
  open window failed after ten minutes, reported as the window having closed, even when asked
  to wait up to an hour. They now wait as long as they ask.
- **Multi-filament prints say they're unverified:** a filament change partway through a print
  hung the H2D, yet the preflight ticked every line of a multi-filament plate. Now it says
  multi-filament printing isn't verified on that printer, and the print needs an extra yes: a
  "Start it anyway" tick in Send to printer, or `multi_filament_ok: true` from an agent (asked
  for before the approval dialog appears, whose title then says so too). Printers whose
  multi-filament prints are verified (`multi_filament` in `profiles/printers.json`) don't ask.

- **Timelapses:** Send to printer has "Record a timelapse" (remembered for the next print), and
  the Printer panel lists the videos on the printer with their thumbnails and the print each
  shows, to download, open or delete. For agents, `printer_files` takes `timelapses`
  (how many thumbnails to attach). Downloads from the printer stream to disk and fail instead of keeping a file
  that arrived short. Another maker's printer refuses `print` with `timelapse` instead of
  ignoring it.
- **Uploads to other makers' printers:** two started at the same moment could share a temporary
  folder, and one printer get the other's G-code. Each now has its own.
- **.scad imports no longer fail now and then on macOS:** OpenSCAD could crash as it started
  (a race in the Qt it ships with), and the import failed with "OpenSCAD failed:" and nothing
  after it. only3d now runs OpenSCAD in the C locale, which takes away what the race needs, and
  a crash is reported as one. OpenSCAD's output is the same in any locale.
- **Print trace as a card:** a print opened in the Prints panel shows how it ended, when it ran
  and for how long, the estimate against what was charged to spools, its objects and filament
  (spool and tray), and the project version, instead of the trace as text. From there you can
  print it again, show the project file, record how it ended and keep a note. A print that
  charged no spool says so.
- **Panels that use their room:** in the side docks a pane whose content is shorter than its
  share lends the rest to the panes that scroll, so a short plate list no longer sits empty
  above a scrolling Inspector; drag a splitter and those panes keep the size you gave them.
  The Inspector leads with the object and its transform, whose fields now span the panel;
  Arrange comes after, shows only the rows the selection can use, spreads its buttons across
  the width, and starts folded for a single object. The Checks pane folds to its tab while
  every check passes and opens when one doesn't (open it yourself and it stays open).
- **Smaller fixes to the panels:** panel titles are cut short before their buttons instead of
  running under them; unticked checkboxes and radio buttons have a visible edge; a click on a
  spool opens its actions (they were only on right-click); the Print panel has Slice and Print
  buttons, and the empty preview a Slice button; the hint line under the view drops whole
  hints to fit instead of ending in "…".
- **Tool forms with fields:** running a tool from the palette, or from a button that opens one
  (temperatures and fans, AMS slots, drying, weighing a spool, adding a printer), now shows its
  arguments as fields built from the tool's schema: numbers with their unit, yes/no, choices
  with what each does, lists, and the optional ones folded under "more options". Access codes
  are typed out of sight. "json" switches to the same arguments as JSON and back. The answer
  shows the tool's text and the data it returned as a tree, with "copy data" for all of it.
- **Printer alerts you can answer:** the Printer panel shows each alert as a card with Bambu's
  description, and the alert that paused a job has the answers Bambu offers as buttons
  (`printer_alert`). The status text block is gone: nozzle sizes sit on the nozzle rows, an
  external spool is listed under the AMS, a printer that doesn't answer is reported once, and
  "copy status" copies the text.
- **Dry filament from only3d:** `printer_ams` actions `dry` and `stop_drying` start or stop
  drying in an AMS 2 Pro or AMS HT, with the command Bambu Studio sends. The temperature and
  time default to Bambu's preset for the loaded filament, and Bambu Studio's limits hold: the
  AMS's temperature range, 1–72 h, a cap during a print, and no start while another loaded
  filament's end would soften and clog its inlet. An agent's start waits for your approval;
  stopping doesn't. The Printer panel has "dry…" and "stop drying" buttons on each AMS that
  dries.
- **Asks before losing changes:** closing the window, quitting, New, Open, Open Recent, or
  opening a project from the file tree, a preview or a drop asks Save / Don't Save / Cancel
  when the project has unsaved changes, like other document apps. Save on a project that has
  no file yet opens the Save panel, and the window closes (or the other project opens) once the
  save lands. ⌘Q (Ctrl+Q elsewhere), Close Window, the window's close button, and on macOS
  quitting from the Dock or at log out all ask. The title bar says "Edited" while there are
  unsaved changes; on macOS the close button shows a dot.
- **Every Bambu Lab printer:** only3d now carries all of Bambu Studio's printer profiles and
  slices for the A1, A1 mini, A2L, P1P, P1S, P2S, X1, X1 Carbon, X1E, X2D, H2D, H2D Pro, H2S,
  H2C and H3C.
  - `set_printer` moves the plate to the printer's own process and filament presets, and the
    plate takes that printer's size and nozzle reach.
  - The start G-code of 12 of them matches Bambu Studio's command line line for line.
  - [docs/printers.md](docs/printers.md) is the public list of what's tested on which printer,
    generated from `profiles/printers.json`.
  - `print` refuses a plate sliced for another model than the printer it goes to, and warns
    before the first print on a printer nobody has printed on with only3d.
  - The X1 and P1's front-left corner, where they wipe the nozzle, is hatched on the plate;
    `check` flags an object in it, and placing and arranging keep out of it.
  - Travels accelerate as Bambu Studio sets them: the first layer's own rate, and gently for a
    short hop onto an outer wall.
- **Other makers' printers:** OrcaSlicer's profiles for about 70 more makers (Prusa,
  Creality, Voron, Anycubic, Elegoo, Qidi, Sovol, Snapmaker...) are built in: 1,183 of their
  1,198 printer presets slice (tools/printers_sweep.py), with the G-code each printer's firmware
  expects (Klipper, Marlin 2, RepRapFirmware, absolute extrusion, firmware retraction, delta
  printers' round plates). [docs/printers.md](docs/printers.md) lists 455 printers.
  - `printer_add` takes Klipper (Moonraker), OctoPrint and PrusaLink printers; `print` uploads
    and starts on them after the same checks and approval, and `printer_status` reads them.
  - The template engine reads PrusaSlicer's and OrcaSlicer's macro statements.
- **Heating asks you:** an agent heating the nozzle or bed now waits for your approval even when
  the printer is idle, as it already did mid-print, so nothing heats up while nobody is watching.
  Turning a heater off still needs no approval.
- **Approvals can't be skipped with positional arguments:** a tool's input must now be an
  object of named arguments. An array was read as the fields in order, so `[null, true]` could
  carry a `confirm` the approval check never saw: an agent could heat, stop a print, delete
  printer files or load filament without asking you, and from a shell start a print.
- **Assemblies:** the new `assembly` tool shows printed parts fitted together with bought
  hardware that is never printed (a parachute, a motor), saved with the project, and reports
  bodies that overlap. `render {"assembly": true, "cut": {"axis": "y"}}` draws a cutaway.
- **Inputs fail where they're wrong:** a bad tool input names the field, down into shape trees
  (`add_object.shape.children[1].size: expected 3 values, got 2`), and says what it takes.
  Per-object settings are checked when saved: Bambu's `brim_type` points to `brim_width`, and
  plate-wide keys point to `set_process`. `settings_catalog` marks the keys an object takes.
- **Print preflight shows the trays:** `print` without confirm lists every AMS and external
  tray as the printer's screen names it (A1, ext left), with the spool's name, colour and what's
  left, and which tray each filament prints from. A tray of another product line or colour than
  the project's is asked about, never ticked; an agent's print stops with
  `filament_choice_needed` until a tray is chosen. The window's Send to printer dialog has a tray
  picker for each filament: a tray that isn't the project's filament starts unpicked, and Start
  print stays off until every filament has a tray and the preflight passed for that choice. A fatal or serious HMS error stops the print
  until it's cleared, and each filament's grams are checked against its spool or the AMS
  reading. `printer_status` names trays the same way.
- **Tree supports, natively:** `support` builds tree supports when the preset asks for them
  (Bambu's do) and straight columns with `support_type: normal`, without a trip through Bambu
  Studio. `support_interface_filament` prints the roof in another filament slot (a support
  material, or PETG under PLA) with no gap, on the H2D's other nozzle, primed on the tower.
- **Fixes:** a per-object `brim_width` now prints (the plate's brim was used before). `check`
  keeps a tip-over or overhang finding as Info, with what covers it, once a brim or supports
  are set. A boolean whose result is empty (a cutter in a tube's hollow) is empty, not an error.
- **ARM64:** Windows and Linux builds for ARM processors.
- **Graphics:** only3d draws with a graphics card when there is one, then a built-in GPU, and
  software only as a last resort. It prefers DirectX 12 on Windows and Vulkan on Linux.
  - `only3d gpu` lists the adapters and marks the one in use.
  - Preferences shows the adapter, and software rendering gets a notice.
- **Idle window:** it no longer redraws four times a second. It wakes for agents, the printer,
  new files and timers that are due, so an idle window uses no CPU.
- **Windows:**
  - Connect an Agent finds Claude Code (`claude.exe`, or `claude.cmd` from npm) and registers
    it.
  - Claude Desktop from the Microsoft Store also gets only3d in its own copy of the config.
  - The installer puts `only3d` on PATH, and the uninstaller takes it off.
  - `only3d mcp` started by a windowed app no longer leaves a console window open.
  - Paths are plain `C:\…`, without the `\\?\` prefix.
- **Wording:** hints and labels say Ctrl, Alt and Shift on Windows and Linux.
- **Crash reports:** a crash leaves a report in `~/.only3d/crashes`, and the next start offers to
  open it or report it on GitHub.
- **Licences:** the apps ship `THIRD-PARTY-NOTICES.txt` with the licence of every package they
  are built from.

## 0.1.0 (2026-11-17)

First pre-release: macOS (universal), Windows (installer and zip) and Linux (AppImage, .deb,
tar.gz), unsigned.
