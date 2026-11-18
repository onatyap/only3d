# Test VMs

Throwaway AWS machines for trying only3d on real Windows and Linux desktops, with and without a
GPU. You (or an agent) drive each one over Amazon DCV, a remote desktop that's free on EC2.

| `vm.sh up …` | Machine | Spot $/h (Frankfurt, Nov 2026) |
|---|---|---|
| `windows cpu` | Windows Server 2025, 4 vCPU, no GPU (`t3.xlarge`) | ~0.10 |
| `windows nvidia` | the same with an NVIDIA T4 (`g4dn.xlarge`) | ~0.25 |
| `windows amd` | an AMD Radeon Pro V520 (`g4ad.xlarge`, Server 2022): no longer offered by AWS | – |
| `linux cpu` | Ubuntu 24.04 with GNOME, no GPU | ~0.08 |
| `linux nvidia` | the same with an NVIDIA T4 | ~0.24 |

Windows Server 2025 is built from the same base as Windows 11 24H2, and its licence is in the
hourly price, so there is no key to buy. Spot machines can be reclaimed mid-test; add
`--on-demand` (about 3x the price) when that matters.

## Every machine deletes itself

- It powers off at its deadline (60 minutes unless you pass `--minutes`). Its shutdown
  behaviour is "terminate", so EC2 then deletes the machine and its disk. The deadline is
  re-checked every few minutes and after every restart.
- An EventBridge schedule terminates it 15 minutes after the deadline, in case the OS hangs.
- `vm.sh down all` ends them all now.

## Use

```bash
export AWS_PROFILE=<your profile>    # its region is used
tools/vmtest/vm.sh setup             # once: SSH key, security group, two IAM roles
tools/vmtest/vm.sh up windows nvidia
tools/vmtest/vm.sh wait only3d-windows-nvidia
tools/vmtest/vm.sh push only3d-windows-nvidia only3d-*-windows-x64-setup.exe
tools/vmtest/vm.sh tunnel only3d-windows-nvidia    # then open https://localhost:8442
```

- **Remote desktop:** the DCV Viewer app (or a browser) on `https://localhost:<port>` while
  `tunnel` runs. DCV has no login of its own: it only listens on the VM itself, and the SSH
  tunnel is the way in.
- **Security group:** it allows SSH from your current address only.
- **GPU drivers:** the instance role lets the VM read AWS's GPU driver buckets and DCV's
  licence, nothing else.
- **Quota:** new AWS accounts have no quota for GPU machines. Ask for
  "Running On-Demand G and VT instances" and "All G and VT Spot Instance Requests" in Service
  Quotas; 8 vCPUs covers two GPU machines at once.

## Things that bit us

- **GPU capacity:** Frankfurt often has no spot `g4dn` capacity. `up` tries every zone, then
  says so; `--on-demand` costs about $0.84 an hour on Windows and $0.66 on Linux.
- **Setup times:** Windows takes about 5 minutes. Linux takes 10-15, longer with the NVIDIA
  driver.
- **DCV certificate:** every VM uses the same certificate (`target/vmtest/dcv.pem`). A browser
  remembers your "continue" per port, so point one tunnel port at whichever VM you're
  looking at.
- **Browser-pane quirk:** in Claude's built-in browser, DCV's web client takes clicks but not
  typing or scrolling. For anything that needs keys, use `shot.ps1` (Windows) or
  `DISPLAY=:0` over SSH (Linux) to run only3d's screenshot self-test with
  `ONLY3D_GUI_OPEN=…`.
- **Agent link:** with a window open on the desktop, `only3d call <tool> -` over SSH reaches
  it. Pass JSON on stdin; PowerShell mangles quotes in arguments.

## What to check

1. `only3d gpu` lists the adapters and marks the one the window uses. On a GPU machine it must
   be the GPU (DirectX 12 or Vulkan); without one, software (WARP on Windows, llvmpipe or
   lavapipe on Linux).
2. Install as a user would: the setup `.exe` (and uninstall), the zip, the `.deb`, the AppImage.
3. In the window: Preferences ▸ Graphics names the same adapter; software rendering shows a
   notice; `ONLY3D_GUI_STATS=1` shows frame times. On NVIDIA, `nvidia-smi` lists only3d.
4. Open an example, slice, preview, export; "Show in Explorer/Files" opens the right folder.
5. With the window open, `only3d call describe` reaches it (the agent link), and
   `only3d mcp install` finds the agent apps.
