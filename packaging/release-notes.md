An early build of only3d: 3D modelling, slicing and printing for Bambu Lab printers, built for
AI agents. Download the file for your computer below.

## Install

**macOS** (Apple silicon and Intel, macOS 11 or later): open `only3d-__VERSION__-macos-universal.dmg`
and drag only3d into Applications. This build isn't signed with an Apple Developer ID yet, so the
first time you open it macOS says it can't check it: open **System Settings ▸ Privacy & Security**,
scroll down and click **Open Anyway**.

**Windows** (10 and 11, 64-bit): run `only3d-__VERSION__-windows-x64-setup.exe`
(`windows-arm64` on ARM PCs such as Snapdragon laptops). It installs for
your user only, with no administrator rights. The installer isn't signed yet, so Windows may say
"Windows protected your PC": click **More info**, then **Run anyway**. It also puts `only3d` on
your PATH, so `only3d` works in terminals you open afterwards. Prefer no installer? Unzip
`only3d-__VERSION__-windows-x64.zip` (or `-arm64.zip`) and open `only3d-app.exe`.

**Linux** (Ubuntu 22.04, Debian 12, Fedora 36 or newer): download the `.AppImage`, make it
executable (`chmod +x only3d-*.AppImage`) and run it. On Debian or Ubuntu you can install the
`.deb` instead: `sudo apt install ./only3d-__VERSION__-linux-amd64.deb`. On ARM (a Raspberry Pi 5,
an ARM laptop) use `only3d-__VERSION__-linux-arm64.deb`, or unpack
`only3d-__VERSION__-linux-aarch64.tar.gz`.

## Connect your agent

In the app, choose **only3d ▸ Connect an Agent** (or use the last step of the first-launch
setup). It works with Claude Code, Claude Desktop, Codex, Gemini CLI, Antigravity, opencode, Kimi
Code, Cursor and any other MCP client.

## Good to know

- Every Bambu Lab printer slices, and about 450 printers of other makers (OrcaSlicer's
  profiles); Klipper, OctoPrint and PrusaLink printers print over the network. Prints have
  been run on the H2D only. [docs/printers.md](https://github.com/onatyap/only3d/blob/main/docs/printers.md) lists
  how far each printer is tested. Starting prints needs the printer in LAN Only mode with
  Developer Mode on.
- Multi-filament prints aren't verified yet: the one tried hung the H2D at its filament change.
  only3d says so before such a print and asks for an extra yes.
- Optional: [OpenSCAD](https://openscad.org) opens `.scad` files, and ffmpeg shows the printer
  camera.
- `SHA256SUMS.txt` lists each file's checksum.
- No telemetry: [what only3d connects to](https://github.com/onatyap/only3d/blob/main/docs/privacy.md).
  [Code signing policy](https://github.com/onatyap/only3d/blob/main/docs/code-signing.md).

What changed: [CHANGELOG.md](https://github.com/onatyap/only3d/blob/main/CHANGELOG.md).

only3d isn't affiliated with Bambu Lab.
