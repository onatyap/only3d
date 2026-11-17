# only3d: notes for agents working on this codebase

## Rules
- Every user-facing action is a tool in `crates/only3d-studio/src/tools.rs` (or `printer.rs`).
  The GUI, the CLI, MCP and the live socket all call `Studio::call`. Never add a GUI-only code
  path that edits `studio.project` directly; add a tool instead.
- Errors are `ToolError { code, message, hint }`. Codes are stable snake_case strings. Hints
  say what to do next.
- Mutating tools are atomic. `Studio::call` snapshots the project and restores it when the
  handler fails. Call `s.changed("tool")` after editing.
- Machine G-code comes from Bambu's templates (`only3d-profiles`). Never hand-write H2D start or
  end sequences.
- `print` must stay gated: a current slice made with Bambu templates, a passing G-code lint,
  an idle printer, and `confirm: true`. Tests must never start a print (`only3d-bambu` refuses
  under `cfg(test)`).

## Commands
- `cargo test --release --workspace --exclude only3d-gui` runs the full suite. The GUI is built
  with `-p only3d-gui`.
- `cargo clippy --release --workspace` must be clean.
- `./target/release/only3d tools --brief` lists the API.
- `only3d call <tool> '<json>' -p x.only3d.json` exercises a tool end to end.
- `cargo run -p only3d-profiles --example render_start` prints the H2D start G-code.
- Don't run `ls` in the shell: it is aliased to `eza`, which hangs. Use `/bin/ls`.
- Release: bump `version` in the root Cargo.toml, merge to main, then push a `v<version>` tag
  (only that tag, never `--tags`). `.github/workflows/release.yml` builds the macOS .dmg, the
  Windows installer and zip, and the Linux AppImage, .deb and .tar.gz, then publishes a
  pre-release. Running it by hand builds the packages without publishing.

## Layout
- `only3d-geom`: mesh, primitives, IO, renderer.
- `only3d-model`: project, shape tree, CSG, checks.
- `only3d-slice`: slicer, G-code, package, lint.
- `only3d-profiles`: Bambu presets and templates.
- `only3d-bambu`: LAN printer link.
- `only3d-studio`: session and tools.
- `only3d-gui`: desktop app.
- `only3d`: the binary.
- `profiles/bambu`: embedded AGPL Bambu presets.
- `examples/`: sample projects.
- `packaging/`: the app icon and per-OS packaging (macOS app and .dmg, Windows NSIS, Linux
  AppImage and desktop entry), used by the release workflow.

## Conventions
- Code runs on macOS, Windows and Linux. Open, reveal and notify go through
  `only3d-gui/src/os.rs`; use `std::env::home_dir()`, never `$HOME`. CI tests Linux and Windows.
- Units are mm, with Z up. Primitives are centred on X/Y and rest on Z=0.
- H2D logical extruder 0 is the left nozzle and 1 is the right. In firmware numbering the
  right nozzle is `T0`. The left nozzle reaches x 0–325, the right x 25–350.
- `infill_density` and similar settings are fractions (0.15), not percentages.
- Objects sit on build plates (`Object::plate`, 1-based). `Studio::plate` is the active plate,
  session state like the selection. `Studio::evaluated()` returns only the active plate's
  objects, so slice, check, print, render and arrange act on it; `evaluated_all()` returns
  every plate. Tools that name an object call `focus_object` to switch to its plate.
