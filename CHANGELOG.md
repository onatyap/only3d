# Changelog

## Unreleased

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
