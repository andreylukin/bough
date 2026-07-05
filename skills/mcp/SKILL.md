---
name: mcp
description: "Call MCP-server tools from run_steps programs (IN DEVELOPMENT — see docs/mcp.md)"
---

# MCP tools (feature in development)

This skill is the planned activation surface for MCP support — the design lives in
`docs/mcp.md` in the bough repo and is not fully implemented yet. Until the `mcp()`
host function ships, tell the user the feature is still being built and do NOT
pretend to call MCP tools.

The intended shape, so partial implementations can be exercised as they land:

- Servers are defined once in a global registry, `~/.bough/mcp/servers.json`
  (`command`/`args`/`env` for stdio, `url` for streamable HTTP; `${VAR}` env
  expansion keeps secrets out of the file).
- A skill grants access by naming servers in its frontmatter (`mcp: [name, …]`);
  invoking the skill connects them, injects their tool list into the prompt, and
  bridges `await mcp(server, tool, args)` into run_steps programs for that turn.
- Every call is gated by Claw Patrol before it executes: verb `mcp:<server>:<tool>`,
  kind from the tool's annotations, unknown fails closed; feed rows and holds work
  like any other egress.
- Stdio servers run Seatbelt-confined with egress forced through the session's
  proxy listener — an MCP server is no more trusted than a bash child.

If the user asks to set up a server now, you can create/edit
`~/.bough/mcp/servers.json` in the shape above so it is ready when the feature
lands — but be explicit that nothing consumes it yet.
