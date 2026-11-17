An early build of only3d: 3D modelling, slicing and printing for the Bambu Lab H2D, built for
AI agents. Download the file for your computer below.

## Install

**macOS** (Apple silicon and Intel, macOS 11 or later): open `only3d-__VERSION__-macos-universal.dmg`
and drag only3d into Applications. This build isn't signed with an Apple Developer ID yet, so the
first time you open it macOS says it can't check it: open **System Settings ▸ Privacy & Security**,
scroll down and click **Open Anyway**.

**Windows** (10 and 11, 64-bit): run `only3d-__VERSION__-windows-x64-setup.exe`. It installs for
your user only, with no administrator rights. The installer isn't signed yet, so Windows may say
"Windows protected your PC": click **More info**, then **Run anyway**. Prefer no installer? Unzip
`only3d-__VERSION__-windows-x64.zip` and open `only3d-app.exe`.

**Linux** (x86-64; Ubuntu 22.04, Debian 12, Fedora 36 or newer): download the `.AppImage`, make
it executable (`chmod +x only3d-*.AppImage`) and run it. On Debian or Ubuntu you can install the
`.deb` instead: `sudo apt install ./only3d-__VERSION__-linux-amd64.deb`.

## Connect your agent

In the app, choose **only3d ▸ Connect an Agent** (or use the last step of the first-launch
setup). It works with Claude Code, Claude Desktop, Codex, Gemini CLI, Antigravity, opencode, Kimi
Code, Cursor and any other MCP client.

## Good to know

- Printer support is the Bambu Lab H2D (0.4 mm nozzles). Starting prints needs the printer in
  LAN Only mode with Developer Mode on.
- Optional: [OpenSCAD](https://openscad.org) opens `.scad` files, and ffmpeg shows the printer
  camera.
- `SHA256SUMS.txt` lists each file's checksum.

only3d isn't affiliated with Bambu Lab.
