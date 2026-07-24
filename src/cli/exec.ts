/**
 * Headless one-shot turn: `bough exec [flags] "do the thing"` (alias: prompt).
 *
 * The prompt comes from the first positional argument, or from stdin when the
 * argument is `-` or absent with stdin piped. Talks to a running bough server —
 * the `bough exec` wrapper auto-starts the default one; invoked directly, this
 * script does not boot anything. It creates a session for the workspace, opens
 * the event stream FIRST (a fast turn must not finish unseen), posts the
 * prompt, streams assistant text to stdout as it arrives, and exits when the
 * turn finishes — 0 for a completed turn, 1 for an errored one, 2 for
 * usage/connection problems.
 *
 *   -w, --workspace DIR   workspace for the session (default: cwd)
 *   -m, --model ID        pin the session's model
 *       --prompt-dir DIR  pin a system-prompt variant (section .md dir) on this
 *                         session — no server restart needed; used by the tuner
 *       --json            suppress streaming; print one result envelope
 *       --timeout SECS    give up after SECS (default 900); exits 1
 *       --port N          server port (default BOUGH_PORT or 4321)
 */
import { parseArgs } from "jsr:@std/cli@1/parse-args";

const args = parseArgs(Deno.args, {
  string: ["workspace", "model", "prompt-dir", "timeout", "port"],
  boolean: ["json"],
  alias: { w: "workspace", m: "model" },
});
let prompt = String(args._[0] ?? "").trim();
if (prompt === "-" || (!prompt && !Deno.stdin.isTerminal())) {
  prompt = (await new Response(Deno.stdin.readable).text()).trim();
}
if (!prompt) {
  console.error(
    'usage: bough exec [-w dir] [-m model] [--json] "..." (or prompt on stdin)',
  );
  Deno.exit(2);
}
const port = args.port ?? Deno.env.get("BOUGH_PORT") ?? "4321";
const api = `http://127.0.0.1:${port}`;
const timeoutMs = Number(args.timeout ?? "900") * 1000;

async function post(path: string, body: unknown): Promise<Response> {
  const res = await fetch(`${api}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${path} → ${res.status} ${await res.text()}`);
  return res;
}

let session: { id: string };
try {
  const workspace = args.workspace ? await Deno.realPath(args.workspace) : Deno.cwd();
  const promptDir = args["prompt-dir"] ? await Deno.realPath(args["prompt-dir"]) : undefined;
  const res = await post("/sessions", {
    title: "exec: " + prompt.slice(0, 48),
    workspace,
    ...(args.model ? { model: args.model } : {}),
    ...(promptDir ? { promptDir } : {}),
  });
  session = await res.json();
} catch (e) {
  console.error(`cannot reach bough server on :${port} — is it running? (${(e as Error).message})`);
  Deno.exit(2);
}

// Stream first, then send: the tail must be attached before the turn can end.
const events = await fetch(`${api}/events?sessionId=${session.id}`);
const reader = events.body!.pipeThrough(new TextDecoderStream()).getReader();

await post(`/sessions/${session.id}/messages`, { text: prompt });

let status = "timeout";
const deadline = Date.now() + timeoutMs;
let buf = "";
let dataName = "";
outer: while (Date.now() < deadline) {
  const { value, done } = await Promise.race([
    reader.read(),
    new Promise<{ value: undefined; done: true }>((r) =>
      setTimeout(() => r({ value: undefined, done: true }), deadline - Date.now())
    ),
  ]);
  if (done || value === undefined) break;
  buf += value;
  const lines = buf.split("\n");
  buf = lines.pop() ?? "";
  for (const line of lines) {
    if (line.startsWith("event:")) dataName = line.slice(6).trim();
    if (!line.startsWith("data:")) continue;
    // The wire frame is `event: <type>` + `data: <whole envelope>`; the event's
    // payload sits at envelope.data.
    let data: Record<string, unknown>;
    try {
      data = (JSON.parse(line.slice(5)) as { data?: Record<string, unknown> }).data ?? {};
    } catch {
      continue;
    }
    if (dataName === "message.delta" && !args.json) {
      const delta = (data as { delta?: string }).delta;
      if (delta) await Deno.stdout.write(new TextEncoder().encode(delta));
    } else if (dataName === "message.part" && !args.json) {
      // A prose() answer block — the marked-up final answer never streams as
      // deltas, so print it whole (raw markdown; styling is the TUI's job).
      const part = (data as { part?: { type?: string; text?: string } }).part;
      if (part?.type === "prose" && part.text) {
        await Deno.stdout.write(new TextEncoder().encode(part.text + "\n"));
      }
    } else if (dataName === "turn.finished") {
      status = String((data as { status?: string }).status ?? "done");
      break outer;
    }
  }
}
reader.cancel().catch(() => {});

// A timed-out turn must not keep running server-side (it would burn tokens and
// skew whatever runs next against this server).
if (status === "timeout") {
  await fetch(`${api}/sessions/${session.id}/interrupt`, { method: "POST" }).catch(() => {});
}

if (args.json) {
  // Post-turn metrics are the authoritative usage record (incl. cache splits
  // for discounted pricing) — the SSE usage event races the finish.
  let usage: Record<string, unknown> = {};
  let turns: Record<string, unknown> = {};
  try {
    const m = await (await fetch(`${api}/sessions/${session.id}/metrics`)).json();
    usage = m.usage ?? {};
    turns = { turns: m.assistantTurns ?? null, tool_calls: m.toolCalls ?? null };
  } catch { /* metrics are best-effort in the envelope */ }
  console.log(JSON.stringify({
    session: session.id,
    status,
    ...turns,
    input_tokens: usage.inputTokens ?? null,
    output_tokens: usage.outputTokens ?? null,
    cache_read_tokens: usage.cacheReadTokens ?? null,
    cache_write_tokens: usage.cacheWriteTokens ?? null,
  }));
} else {
  // Deltas usually end mid-line; land the shell prompt cleanly.
  console.log();
}
Deno.exit(status === "done" || status === "stopped" ? 0 : 1);
