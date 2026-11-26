# Release readiness

How we decide that only3d is ready to be public, and where it stands. Each item has a target, a
way to check it, and the latest result. An item scores 0 (missing), ½ (partly there) or 1 (meets
its target), times its weight. Every item says what was checked and when.

**Ready to go public** means all of these:
- a total of at least 85 out of 100;
- full marks on the safety items (R1, R5);
- no item at 0, unless it is written down here as out of scope for the first public release.

Numbers come from `python3 tools/readiness/bench.py` (slicing against Bambu Studio's own command
line, imports, MCP latency, the window's startup and idle cost, the GPU viewport), the throwaway
VMs in `tools/vmtest` (real Windows and Linux, with and without an NVIDIA GPU), CI, and the agent
evals in `evals/`.

## Score

| Round | Date | Value /40 | Performance /25 | Platforms /20 | Public /15 | Total |
|---|---|---|---|---|---|---|
| 1 | 2026-11-18 | 24.5 | 23.5 | 14 | 8 | **70** |
| 2 | 2026-11-18 | 28 | 23.5 | 16.5 | 13 | **81** |
| 3 | 2026-11-18 | 34 | 25 | 17.5 | 13 | **89.5** |
| 4 | 2026-11-18 | 34 | 25 | 17.5 | 13 | **89.5** |
| 5 | 2026-11-26 | 34 | 25 | 17.5 | 13 | **89.5** |

## 1. Value: the journeys that matter (40)

The 20% of features that carry 80% of the value: get it running, let an agent design a part,
slice it right, print it safely, and connect whichever agent you use.

| ID | Journey | Weight | Target | Result | Score |
|---|---|---|---|---|---|
| V1 | Install and first run | 5 | Download, install, open, set up, on macOS, Windows and Linux, with no code or Cargo | DMG, Windows installer and zip, AppImage, .deb, tar.gz all install and open (VMs, 2026-11-17). The Windows installer adds `only3d` to PATH and removes it on uninstall (2026-11-18) | 1 |
| V2 | An agent designs a part | 7 | Eval suite ≥ 90% passing, no tool errors | 13/13 with the current code (2026-11-18, $4.30). One tool error came from a guessed preset name, now accepted; the other was the eval sandbox blocking a file read | 1 |
| V3 | Import, check, slice, export | 6 | STL, 3MF, OBJ, STEP and OpenSCAD all reach a slice; the G-code check passes; the same G-code on every OS | All five formats slice (2026-11-18). The tray's G-code is byte-identical on macOS, Windows and Linux (2026-11-17) | 1 |
| V4 | Estimates you can trust | 4 | Time and grams within ±10% of Bambu Studio on the reference models | Tray −4.5% time, +2% grams; tile −9%, −1%; 1M-triangle organic mesh −1%, −2% (2026-11-26) | 1 |
| V5 | Print and watch, safely | 6 | A print starts only after the user approves it in the window; status, camera and alerts work on the H2D | Single-filament prints, pause and resume verified on the H2D (2026-11-15). The one multi-filament print tried hung the H2D at its filament change (F-20, 2026-11-22; cause unknown, the change block follows Bambu's template). Until it's fixed a multi-filament print says it's unverified and needs an extra yes (F-21). A print can't be started twice (F-32), and an approval request sends a notification and says so when it lapses (F-29) (2026-11-25) | ½ |
| V6 | Agents connect on every OS | 6 | Claude Code, Claude Desktop, Codex, Gemini, Cursor, opencode and Kimi register in one click on macOS, Windows and Linux | macOS: used daily. Windows (VM, 2026-11-18): Claude Code registers with a plain path, `claude mcp list` shows ✓ Connected, `only3d mcp` drives the open window, and no console window appears when a windowed app starts it. Linux (Ubuntu 24.04 ARM64, the released .deb, 2026-11-18): Claude Code ✔ Connected, Codex enabled, Gemini CLI ✓ Connected in a folder it trusts (only3d now says so when it connects Gemini). Claude Desktop's Store copy on Windows is unit-tested only | 1 |
| V7 | Every printer | 6 | Slice, export and print on every Bambu printer, then other makers'; a public registry (docs/printers.md) says what's checked against the maker's slicer and what has printed on real hardware | All 15 Bambu printers slice with their own presets, plate and nozzles (2026-11-18). `tools/printers_verify.py` compares each with Bambu Studio's command line: the start G-code of 12 is identical; the A1, A1 mini and A2L differ in one M201 line (Bambu's command line writes 6000 mm/s², their profiles 12000–20000), and their time estimates run ~20% under Bambu's (others 4–12%). [docs/printers.md](printers.md) is generated from `profiles/printers.json`, and CI checks it. `print` refuses a plate sliced for another model and warns before a first print on an untested printer. Other makers (round 4, 2026-11-18): OrcaSlicer's profiles for 70 makers built in; 1,189 of 1,198 printer presets slice cleanly (`tools/printers_sweep.py`; the rest are food and pellet printers' 3–8 mm nozzles), with each firmware's G-code, and every template of every maker parses; the registry lists 455 printers. Klipper (Moonraker), OctoPrint and PrusaLink printers print after the same checks and approval (tested against a stand-in server, not a real printer). Not yet compared with OrcaSlicer's own output (running it here was blocked). Printed on the H2D only | ½ |

## 2. Performance, for any task (25)

| ID | Measure | Weight | Target | Result (M3 Max unless noted) | Score |
|---|---|---|---|---|---|
| P1 | Slice time | 5 | No slower than Bambu Studio's command line on the same presets | Tray 0.36 s vs 0.69 s (warm; the first run after a build took 1.6 s); tile 0.16 s vs 0.12 s; 1M-triangle mesh 0.92 s vs 2.47 s, with ⅙ of the CPU time (2026-11-26) | 1 |
| P2 | Peak memory while slicing | 2 | No more than Bambu Studio | Tray 131 MB vs 201 MB; 1M-triangle mesh 465 MB vs 699 MB | 1 |
| P3 | Viewport on a GPU | 4 | 1M triangles and 2M toolpath segments under 16 ms a frame | 1.3 ms and 4.1 ms (M3 Max); 1.4 ms for the tray scene on an NVIDIA T4 (Linux VM) | 1 |
| P4 | Without a GPU | 3 | Software rendering is picked by itself and stays usable (under 100 ms a frame for the tray) | Picked by itself on Windows (WARP) and Linux (lavapipe, X11 and Wayland), and it says so. Software adapters skip MSAA. `only3d gpu --bench` on lavapipe, 4 cores (Linux ARM64, Docker, 2026-11-18): a selected tray-sized part (10k triangles) 81 ms a frame at 2400×1500 and 64 ms at 1200×750, waiting for each frame (it was ~0.4 s before MSAA was dropped). The 1M-triangle mesh takes 129 ms and 2M toolpath segments 634 ms: big models are slow without a GPU | 1 |
| P5 | Idle window | 3 | Almost no CPU when nothing changes | 0.00 s of CPU per 10 s (was 0.22 s); wakes for agents, printer events and new files | 1 |
| P6 | Startup | 2 | Window drawn within 2 s | 0.82 s to the first screenshot | 1 |
| P7 | Agent round trip | 3 | A read tool under 5 ms; the tool list within budget (the MCP test's cap, 60,500 characters) | `describe` 0.71 ms; tool list 60,491 characters (2026-11-26): within the cap, but the next tool needs room made first | 1 |
| P8 | Big imports | 3 | A 1M-triangle STL loaded and checked under 2 s | 0.18 s (2026-11-26) | 1 |

## 3. Platforms and hardware (20)

| ID | Platform | Weight | Target | Result | Score |
|---|---|---|---|---|---|
| X1 | macOS, Apple silicon | 3 | Everything works | Used daily | 1 |
| X2 | macOS, Intel | 2 | The universal app slices and draws on Intel | The x86_64 half, run under Rosetta: byte-identical G-code, draws with Metal (2026-11-18). No Intel GPU tried (see X5) | 1 |
| X3 | Windows 10/11, x64 | 4 | Installs, draws on any GPU, agents connect | VMs: installer, PATH, uninstall, WARP and NVIDIA, agent link, Claude Code (2026-11-17/18) | 1 |
| X4 | Linux x64 | 3 | X11 and Wayland desktops | Ubuntu 24.04 GNOME on X11 (VMs); a Weston Wayland session (Docker, 2026-11-18) | 1 |
| X5 | GPU makers | 5 | Apple, NVIDIA, AMD and Intel GPUs used when present; software rendering otherwise | Apple, NVIDIA (Windows and Linux) and software verified. AMD: AWS no longer offers its AMD machines (g4ad); Azure's NVads V710 is the remaining option. Intel: no cloud machine has one | ½ |
| X6 | ARM64 Windows and Linux | 1 | Native builds | Both built by the release workflow (2026-11-18). Linux ARM64: the .deb installs and runs on Ubuntu 24.04, its G-code is byte-identical to macOS's, and agents connect (Docker on Apple silicon). Windows ARM64: built, not yet run | 1 |
| X7 | CI | 2 | Tests and clippy on Linux and Windows on every push; macOS locally | Yes | 1 |

## 4. Ready for the public (15)

| ID | Item | Weight | Target | Result | Score |
|---|---|---|---|---|---|
| R1 | No personal data | 3 | No printer IDs, access codes, home paths or personal models in the tree or its history | None in the tree (`tools/readiness/audit.sh`, 2026-11-26); CI runs the same audit and fails on home folders and private addresses. Every commit on `origin/main` uses the GitHub noreply address. Some local branches (`fuzzy-skin`, `refs/backup/*`) and the `baseline` tag carry a private address and must never be pushed: push only `main` and one `v<version>` tag | 1 |
| R2 | Licences | 2 | AGPL kept; Bambu's profiles credited; third-party notices ship with the apps | All dependencies are permissive and AGPL-compatible. THIRD-PARTY-NOTICES.txt (541 packages, Bambu credited) is in the DMG, installer, zip, AppImage, .deb and tar.gz (CI, 2026-11-18) | 1 |
| R3 | Docs | 2 | README, guide, CONTRIBUTING, SECURITY, CHANGELOG, issue templates | All there | 1 |
| R4 | Trusted installs | 2 | Signed on Windows, notarized on macOS | Unsigned (macOS ad hoc); the release notes say how to open it. **The owner decided (2026-11-26) that builds are signed before the repository goes public.** The release workflow signs and notarizes once the certificates are in the repository secrets ([packaging/README.md](../packaging/README.md)): Developer ID signing tried on macOS with a stand-in certificate (hardened runtime, timestamp, the app runs), Windows signing on CI with a throwaway certificate. Waiting for an Apple Developer account and a Windows code-signing certificate | 0 |
| R5 | Safety | 3 | Printing gated (current slice, Bambu templates, lint, idle printer, the user's approval); access codes stay local | Gated and tested; access codes in ~/.only3d with mode 600. A multi-filament plate, never verified on a printer (V5), is no longer offered as ready: the preflight says so and the print needs an extra yes (F-21, 2026-11-24). `print` refuses a plate the printer is already printing or was just sent (`already_started`, F-32), and an agent whose link drops mid-approval is told the action may have been done. Approval requests notify the user and lapse after 15 minutes with "not done" (F-29) (2026-11-25) | 1 |
| R6 | Crash reports | 1.5 | A crash leaves a report, and the next start offers it | A panic writes ~/.only3d/crashes/crash-*.txt; the next window offers to open it or file it on GitHub (tested) | 1 |
| R7 | Updates | 1.5 | Users find out when a new version is out | A once-a-day check against GitHub releases shows a status-bar chip; Preferences turns it off | 1 |

## Verdict (round 5)

89.5 of 100, unchanged, with full marks on R1 and R5. Round 5 (2026-11-26) adds the safety
fixes found while printing on the H2D: F-21 (multi-filament marked unverified), F-32 (no
second start) and F-29 (approvals you can't miss). The open bug that matters is F-20: a
filament change hung the H2D, so V5 stays at ½ until a multi-filament print runs with the user
there. Slicing, estimates, imports and agent latency were re-measured (`bench.py --no-gui`,
2026-11-26) and still meet their targets; the window's startup and idle cost (P5, P6) and
memory (P2) keep their round-4 numbers. R4 (signing) is the one item at 0, and the owner wants
it done before going public: the release workflow is ready to sign and notarize once the
certificates are added.

Done for going public, apart from the owner's steps below: the public release brought up to date
(0.2.0, with the ARM64 builds the README lists), the repository description, `main` protected
against force-pushes and deletion, signing ready in the release workflow, and this rescoring.

What only the owner can do: the signing certificates (an Apple Developer ID and a Windows
code-signing certificate, added as repository secrets) and a release built with them; make the
repository public, then turn on private vulnerability reporting (SECURITY.md points there;
GitHub only allows it on a public repository); the multi-filament print at
the H2D (V5, F-20); a first print on another maker's printer (V7); GPU quota for an AMD
machine (X5); allowing a reference run of OrcaSlicer.

## Verdict (round 4)

89.5 of 100, with full marks on R1 and R5. Round 4 added every other maker's printers (V7's
target also needs real prints on them, so its score stays ½). The one item at 0 is R4
(signing): it counts as out of scope only if the owner agrees, since free Windows signing needs
the repo public first and notarizing needs a paid Apple account. If they agree, only3d meets
this framework's bar for going public; otherwise signing comes first.

What only the owner can do: decide R4, approve merging to main and making the repo public, a
multi-filament print at the H2D (V5), a first print on another maker's printer (V7), GPU quota
for an AMD machine (X5), and allowing a reference run of OrcaSlicer to compare other makers'
G-code.

## Next

What would raise the score most, roughly in order of value for effort:

1. V5 a multi-filament print, at the printer with the user.
2. V7 a first print on another Bambu printer; the A-series M201 limit and time estimates; then
   other makers (Prusa, Moonraker/Klipper, OctoPrint).
3. X5 an AMD GPU (Azure NVads V710) and an Intel one.
4. R4 signing: add the certificates as secrets (packaging/README.md) and cut a signed release.
5. X6 run the Windows ARM64 build on ARM hardware.
6. P7 make room in the tool list (9 characters under its cap) before the next tool.
