# Privacy

only3d has no account, no telemetry and no analytics. Your projects, settings, spools and print
history stay on your computer (in `~/.only3d`, and wherever you save projects).

It connects to other computers only in these cases:

| What | Where it connects | What is sent | How to stop it |
|---|---|---|---|
| Checking for a new version | `api.github.com`, when the window opens, at most once a day | A request for only3d's release list, with `only3d/<version>` as the user agent. Nothing about you or your projects | Preferences ▸ turn off the update check |
| Printers you add | Each printer's address on your network (Bambu printers over MQTT, FTPS and the camera port; Klipper, OctoPrint and PrusaLink over HTTP) | Print files, commands and status requests, and the printer's access code or API key you entered | Delete its entry from `~/.only3d/printers.json` |
| Finding printers | Your local network: it listens for printers announcing themselves (SSDP) when you search. When a saved Bambu printer stops answering, it also tries port 8883 on the addresses of your own subnet and the printer's old one, to find the printer by its certificate | Nothing beyond the start of a TLS connection | Don't search; add printers by address |
| Phone notifications | The URL you enter in Preferences (for example an ntfy topic) | "Print finished: <job name>" | Leave the URL empty (the default) |
| Crash reports | Nowhere by itself. After a crash the window offers to open a GitHub issue in your browser | Only what you choose to paste into the issue | Don't file it |
| Agents (MCP) | Agents on your computer, through a local socket | Your project and printer state, to the agents you connect | Don't connect an agent, or remove it from the agent's settings |

**Connect an Agent** adds only3d to the settings file of the agent you choose (Claude Code,
Codex, Cursor and others). The Windows installer adds only3d's folder to your PATH; uninstalling
takes it out again.

Agents you connect are separate programs with their own privacy policies: what you tell them,
and what only3d tells them, goes to whichever AI service they use.
