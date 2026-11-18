# Security

only3d drives a real printer on your network, so security problems matter. Please report them
privately: use **Report a vulnerability** on the repository's Security tab (GitHub private
vulnerability reporting), not a public issue.

Worth reporting, for example:
- anything that starts, stops or changes a print without the user's approval in the window;
- a way to read or leak a printer's access code (kept in `~/.only3d/printers.json`);
- other users or programs on the same machine or network driving only3d through its live
  socket or MCP server;
- G-code that only3d would send although its checks should have stopped it.

You'll get an answer within a week. Fixes go out as a new release, with credit if you want it.
