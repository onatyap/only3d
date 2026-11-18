# Contributing

Thanks for helping. only3d is young, so issues that say what you tried and what happened are as
useful as code.

## Reporting a problem

Open an issue with the bug form. It asks for:
- your version (`only3d --version`);
- your system;
- your graphics adapter (`only3d gpu`);
- what you did.

If only3d crashed, it saved a report in `~/.only3d/crashes` and offered it the next time it
started; paste it in.

## Building

You need Rust (the version is pinned in `rust-toolchain.toml`; `rustup` installs it).

```bash
cargo build --release -p only3d                              # the program, with the window
cargo test --release --workspace --exclude only3d-gui        # the full suite
cargo clippy --release --workspace --all-targets -- -D warnings
```

On Linux, install the build packages first (Debian and Ubuntu names): `pkg-config
libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev libgl1-mesa-dev libdbus-1-dev`.

## How the code is put together

[CLAUDE.md](CLAUDE.md) is the short version, written for agents and people alike: the rules,
commands and crate layout. The ones that matter most:

- Every user-facing action is a tool (`crates/only3d-studio/src/tools.rs`). The window, the
  command line, MCP and the live socket all call the same tools; don't add a window-only path.
- Errors carry a stable code and a hint that says what to do next.
- Printing stays gated: a current slice made with Bambu's templates, a passing G-code check, an
  idle printer, and the user's approval. Tests never start a print.
- Machine G-code comes from Bambu's templates; never hand-write start or end sequences.

## Checking your change

- `python3 tools/readiness/bench.py` measures slicing (against Bambu Studio when it's
  installed), imports, agent latency, startup and the viewport.
- `tools/vmtest` makes throwaway Windows and Linux machines on AWS, with or without a GPU.
- `evals/run.py` runs real agents against design tasks.
- [docs/readiness.md](docs/readiness.md) is the scorecard for the first public release.

## Licence

only3d is under the GNU AGPL 3.0 or later. By contributing you agree that your contribution is
under the same licence.
