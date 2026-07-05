# MCP support — design draft

Status: phases 1–2 implemented (src/mcp/ — registry, stdio client, seatbelt spawn,
call-layer gating, /mcp builtin, vm bridge, endpoints; remote servers via the
official SDK's Streamable HTTP transport + OAuth/PKCE with bough-hosted callback,
tokens under ~/.bough/mcp/tokens/). Phases 3–4 (per-server ops tables, UI) remain.
One deliberate delta from the design below: the remote JSON-RPC channel goes
DIRECT, not through the session proxy — routing it would double-gate every
tools/call POST (held at the HTTP layer on top of the mcp-verb layer) and park
turn-start connects on approvals. The call-layer gate is the border; channel
attribution needs a policy carve-out for granted remote hosts first (deferred).

## Shape in one paragraph

MCP servers are a new capability bridged into the sealed run_steps VM, on the same
trust model as everything else the sandbox can do: the program calls one new host
function, `await mcp(server, tool, args)`, and the real work happens on the host,
where stdio servers run Seatbelt-wrapped with their egress forced through the
session's Claw Patrol listener — and every tool call passes through the Claw Patrol
policy engine (`decide()`) before it executes, as a first-class verb with feed rows
and holds. Skills are the activation surface: a skill's
frontmatter names the servers it needs, and invoking `/skill` is what connects them,
injects their tool list into the supervisor prompt, and bridges `mcp()` for that
turn — no active skill, no MCP capability. Servers are defined once in a global
registry; skills reference them by name.

## Goals / non-goals

Goals (v1):

- Call MCP tools from inside the sealed sandbox — one host function, string-only
  postMessage protocol preserved (JSON round-trip like `agent()`).
- Confine the servers themselves: an MCP server is no more trusted than a bash
  child. Seatbelt profile (writes → workspace + session snapshot dir), loopback-only
  network when the proxy is up, egress attributed to the session in the net feed.
- Gate the tool calls: every `mcp()` call is classified and routed through the
  session's Claw Patrol `decide()` **before** it executes — verb
  `mcp:<server>:<tool>`, kind from MCP annotations, unknown fails closed. Feed
  rows, holds, and per-branch policy come from the existing engine.
- Skills as the grant: `mcp:` in SKILL.md frontmatter is the only way a turn gets
  MCP tools. The set of callable servers is exactly the union of the invoked
  skills' references.

Non-goals (v1): MCP resources/prompts (tools only), OAuth'd remote servers, bough
acting as an MCP server, exposing MCP to subagent turns (same depth-1 posture as
delegation — revisit).

## Server registry

`~/.bough/mcp/servers.json`, zod-validated like `net/config.ts` (a corrupt file
falls back to empty, never half-loads):

```json
{
  "servers": {
    "chrome-devtools": {
      "command": "npx",
      "args": ["chrome-devtools-mcp@latest"],
      "env": { "DEBUG": "0" }
    },
    "linear": { "url": "https://mcp.linear.app/sse" }
  }
}
```

- `command`/`args`/`env` → stdio transport; `url` → streamable HTTP/SSE. Exactly one.
- `env` values support `${VAR}` expansion from bough's own environment so secrets
  live in `~/.bough/env`, not in the registry file. Secrets reach the **server
  child process only** — never the VM, never the model's context.
- API: `GET /mcp/servers` (registry + per-session connection status),
  `PUT /mcp/servers` (validate, persist, drop live connections to changed entries).

## Skill reference

`skills.ts` frontmatter grows one key (the parser is already line-based; this is a
comma list, same style as everything else):

```
---
name: browse
description: Drive Chrome via the devtools MCP
mcp: chrome-devtools
---
```

- `Skill` gains `mcp: string[]`. Builtins may reference servers too.
- `activeFor()` currently returns just the prompt string; it grows a structured
  sibling `activeSkills(message): { sections: string; servers: string[] }` (or
  `activeFor` returns both) so turn.ts learns which servers to wire without
  re-parsing.
- A skill referencing an unregistered server still loads; the prompt section notes
  "server X is not configured" and `mcp()` calls to it reject. The skill degrades,
  it doesn't vanish.

## Host function bridge

One new optional host function, wired exactly like delegation:

- `vm_worker.ts`: add `"mcp"` to `HostName`; sandbox-side
  `mcp = async (server, tool, args) => JSON.parse(await hostCall("mcp", [server, tool, JSON.stringify(args ?? {})]))`.
  Passed as a new AsyncFunction parameter.
- `vm.ts` `HostFns`: `mcp?(server: string, tool: string, argsJson: string): Promise<string>`.
- `run_steps.ts`: bridge only when `ctx.mcp` is present (like `ctx.delegate`).
  MCP calls can be slow (browser automation, long fetches) — when `mcp` is bridged,
  use the same enlarged wall-clock budget as delegation (`DELEGATING_TIMEOUT_MS`).
- `ToolRunCtx` gains:

```ts
mcp?: {
  /**
   * Call a tool on an activated server. Rejects for servers outside the turn's
   * grant, and runs the call through the session's Claw Patrol decide() first —
   * a deny rejects with the policy reason, a hold blocks on human approval.
   */
  call(server: string, tool: string, args: unknown): Promise<unknown>;
};
```

Result mapping: MCP `content` blocks → text blocks concatenated; structured
`structuredContent` preferred when present; `isError: true` → throw (rejects inside
the program as an ordinary exception, per the tool contract).

## Connection manager — `src/mcp/manager.ts`

Use the official SDK (`npm:@modelcontextprotocol/sdk`, works under Deno) rather
than hand-rolling JSON-RPC.

- Connections keyed by `(sessionId, serverName)`. Per-session, not global, because
  confinement is per-session: the Claw Patrol listener, the seatbelt workspace, and
  the snapshot dir all differ.
- **Eager connect at turn start**: when the triggering message activates skills
  with `mcp:` refs, turn.ts asks the manager to `ensure(sessionId, servers)` before
  building the system prompt — connect (or reuse), `tools/list`, cache the tool
  catalog. A server that fails to connect yields a prompt note instead of tools.
- Cached across turns; closed on session end, registry change, server process
  exit, or an idle TTL (~30 min, same spirit as plugin activations).
- `tools/list` re-fetched on reconnect only; `listChanged` notifications can
  refresh the cache later.

### Confinement (the point)

stdio servers spawn exactly like bash children (`tools/bash.ts` is the template):

```ts
const netEnv = await clawpatrolEnv(sessionId);
let argv = [cfg.command, ...cfg.args];
if (sandboxed && Deno.build.os === "darwin") {
  argv = wrap(argv, {
    workspace,                       // session workspace = the rw root
    allowWrite: [sessionDir],        // clonefile snapshot dir
    confineNetwork: Object.keys(netEnv).length > 0,  // loopback-only when proxy is up
  });
}
// cwd = session workspace; env = minimal (PATH, HOME) + expanded registry env + netEnv
```

Consequences, all intended:

- The server's egress goes through the session's intercepting proxy: rows in the
  net feed, per-branch policy, holds/denials — MCP traffic is Claw Patrol traffic.
- With the proxy up, loopback confinement means a server that ignores proxy env
  fails closed (no open-internet side door). Known caveat carried over from bash:
  Go binaries on macOS ignore the MITM CA env — same limitation, same answer.
- The server can only write where the sandbox could write anyway.

Live-proven caveats for Node-based servers (exa-mcp-server, 2026-07-05):

- Node's global `fetch` (undici) ignores proxy env by default — the server's API
  fetch fail-closed against the loopback seatbelt ("fetch failed"). Fix: add
  `"NODE_USE_ENV_PROXY": "1"` (Node 24+) to the server's registry `env`.
- `npx -y <pkg>` re-checks the npm registry even with a warm cache; off-allowlist
  that hold outlives the 30s initialize window and the connect dies (gracefully —
  the prompt says UNAVAILABLE). Register with `npx -y --prefer-offline <pkg>`.

Remote (`url`) servers: dial through the session proxy too — hand the SDK transport
a custom fetch built on `Deno.createHttpClient({ proxy: { url: sessionListener } })`
with the Claw Patrol CA trusted, so the JSON-RPC channel itself is attributed in
the feed. Note this channel-level routing is attribution, not the gate — the
JSON-RPC POST is opaque to the classifier; the enforcement point for remote
servers is the call-layer `decide()` (next section), which runs before the request
is ever sent.

## Prompt surface

When servers are active, the system prompt (next to the Active skill sections)
gains:

```
# MCP tools
Skills activated MCP servers for this turn. Call them from your program with
await mcp(server, tool, args) — returns the tool's result (JSON), throws on error.

server "chrome-devtools":
- take_snapshot() — Capture a text snapshot of the page
- click({uid}) — Click the element with the given uid
- navigate_page({url, timeout?}) — ...
```

Compact by design: name, one-line description, required/optional param names from
the input schema. No full JSON Schema dumps — the catalog is capped (~4k chars per
server, truncation noted) so a chatty server can't crowd out the task.

## Session management — the `/mcp` builtin skill

Same pattern as the `theme` and `net-plugin` builtins: `/mcp <intent>` (e.g.
`/mcp status`, `/mcp restart chrome-devtools`, `/mcp reauth linear`) pulls a
builtin skill whose body instructs the supervisor to drive the management API on
`http://127.0.0.1:${BOUGH_PORT:-4321}` (loopback bypasses the egress proxy;
`$BOUGH_SESSION` is in the shell env). The skill is the UX; the endpoints do the
work:

| endpoint | effect |
|---|---|
| `GET /mcp/servers?session=` | registry + per-session status: connected / error (with last error), tool count, activation source (which skill, or manual), uptime |
| `POST /mcp/servers/:name/restart?session=` | drop the `(session, server)` connection — kill the stdio child / close the transport — reconnect, re-run `tools/list`, return the new status |
| `POST /mcp/servers/:name/enable?session=` | manual per-session activation, body `{"ttl":"2h"}` optional — exact parity with plugin activations (lapses fail closed) |
| `POST /mcp/servers/:name/disable?session=` | drop the activation and the connection |
| `POST /mcp/servers/:name/auth?session=` | (phase 2, with remote servers) start the OAuth flow: returns the authorization URL for the human to open; tokens persist under `~/.bough/mcp/tokens/` (0600), reach only the transport, never the sandbox |
| `DELETE /mcp/servers/:name/auth` | clear stored credentials ("logout") |

Skill body contract: ground in `GET /mcp/servers` first, do the one thing asked,
then re-GET and report the resulting status (the same probe-then-prove shape as
`net-plugin`). A crashed server surfaces here too — the manager marks the
connection errored, the prompt section says so, and `/mcp restart <name>` is the
human's fix.

Note `enable` resolves the "activation without a skill" open question without
breaching the grant model: the grant still enters through a human-typed `/`
invocation — `/mcp enable x` is explicit and per-session, it just doesn't require
authoring a skill folder first.

## Gating tool calls — the Claw Patrol border (v1, not optional)

The seatbelt + proxy confinement borders the MCP server's **own process** — and
that is not where MCP effects happen:

- A stdio server's effects routinely escape over loopback, which the profile must
  allow: chrome-devtools drives a real Chrome over CDP, and Chrome — unsandboxed,
  not behind the proxy — does the actual fetching. Same for anything talking to a
  local daemon (Docker, a dev server, launchd services).
- A remote server executes effects **server-side**; the only thing crossing the
  proxy is one opaque JSON-RPC POST the classifier can't tell a read from a
  refund. It would gate at "generic POST" fidelity — noise for reads, blindness
  for the interesting verbs.

So the tool call is the only point where the action is legible, and that is where
the border runs: `ctx.mcp.call` classifies the call as a pseudo-request — host =
the server name, verb = `mcp:<server>:<tool>`, kind seeded from MCP tool
annotations (`readOnlyHint` → read, `destructiveHint`/absent-read-only → write,
no annotations → unknown, which **fails closed** in read_only/review exactly like
the plugin tables; annotations are server-supplied hints, so treat them as a
classification aid, never a trust grant) — and routes it through the same
`decide()` **before** the SDK call is made. Allow → execute; deny → the program's
`mcp()` rejects with the policy reason; hold → the call parks in the existing
approval UI and executes (or rejects) on the human's verdict, same as a held HTTP
request. Every call lands in the net feed either way, attributed to the session.

Per-branch targeting comes free: `holdVerbs`/`denyVerbs` in the branch policy can
name `mcp:chrome-devtools:navigate_page` today's editor already edits. The
process-level confinement stays — it's what keeps a server from side-stepping the
call-layer gate with its own direct egress — but it's the backstop, not the border.

## Files

| file | change |
|---|---|
| `src/mcp/config.ts` | registry schema, load/persist, `${VAR}` expansion |
| `src/mcp/manager.ts` | per-session connections, seatbelt spawn, tool catalog, TTL |
| `src/mcp/gate.ts` | tool call → pseudo-request (verb/kind from annotations) → `net.decide()`, feed rows |
| `src/mcp/prompt.ts` | catalog → capped prompt section |
| `src/supervisor/skills.ts` | parse `mcp:` frontmatter; expose per-skill servers; `/mcp` builtin |
| `src/turn.ts` | resolve active servers → `manager.ensure()` → `toolCtx.mcp` + system section |
| `src/tools/run_steps.ts` | bridge `mcp` host fn when `ctx.mcp` present; enlarged timeout |
| `src/harness/vm.ts` / `vm_worker.ts` | `mcp` in HostFns/HostName, JSON round-trip |
| `src/tools/types.ts` | `ToolRunCtx.mcp` |
| `src/server/app.ts` | `GET/PUT /mcp/servers`, restart / enable / disable / auth endpoints |

## Testing

- Fixture stdio server (a tiny Deno script speaking MCP over stdio: `tools/list` +
  an `echo` tool) checked in under `src/mcp/testdata/` — manager tests spawn it for
  real (self-skip without `--allow-run`, like the sandbox tests).
- skills.test.ts: `mcp:` frontmatter parsing, unknown-server degradation.
- run_steps.test.ts: program calls `mcp()` against a stubbed `ctx.mcp`; JSON
  round-trip; rejection for non-granted server.
- Confinement probe (pattern from `containment_probe.test.ts`): fixture server
  attempts an off-workspace write and a direct (non-proxy) fetch; both must fail
  when sandboxed.

## Phases

1. Registry + manager + stdio spawn + host fn + skills wiring, **with call-layer
   gating from day one**: every `mcp()` call through `decide()` (verb + kind +
   feed row + hold/deny), servers confined via proxy env + loopback seatbelt as
   the backstop. Shipping ungated MCP even briefly would punch a hole in the
   border — the gate is not a fast-follow. Includes the `/mcp` builtin
   (status / restart / enable / disable).
2. Remote HTTP servers, JSON-RPC channel proxied for attribution; OAuth +
   `/mcp reauth`.
3. Richer classification: per-server ops tables (plugin-style) overriding
   annotation-seeded kinds, e.g. marking a nominally-read tool destructive.
4. UI: server status card in the right rail; approvals already work via the
   existing net feed/hold flow from phase 1.

## Open questions

- ~~Should a session be able to activate a server without a skill?~~ Resolved:
  `/mcp enable <name>` (see the `/mcp` builtin) — manual, per-session, optionally
  TTL'd, still entered through an explicit human `/` invocation.
- Subagents: task strings can't invoke skills today, so subagent turns get no MCP.
  Fine for v1; revisit if delegated tasks need browser/CRM access.
- Per-skill server definitions (an `mcp.json` inside the skill folder) would make
  skills self-contained/shareable; deferred to keep one registry and one secrets
  story.
