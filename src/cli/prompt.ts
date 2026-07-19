/**
 * Headless one-shot turn: `bough prompt [flags] "do the thing"`.
 *
 * Talks to the RUNNING bough server (it does not boot one): creates a session
 * for the workspace, opens the event stream FIRST (a fast turn must not finish
 * unseen), posts the prompt, streams assistant text to stdout as it arrives,
 * and exits when the turn finishes — 0 for a completed turn, 1 for an errored
 * one, 2 for usage/connection problems.
 *
 *   -w, --workspace DIR   workspace for the session (default: cwd)
 *   -m, --model ID        pin the session's model
 *       --yolo            auto-approve this session's network holds
 *       --json            suppress streaming; print one result envelope
 *       --timeout SECS    give up after SECS (default 900); exits 1
 *       --port N          server port (default BOUGH_PORT or 4321)
 */
import { parseArgs } from "jsr:@std/cli/parse-args";

const args = parseArgs(Deno.args, {
  string: ["workspace", "model", "timeout", "port"],
  boolean: ["yolo", "json"],
  alias: { w: "workspace", m: "model" },
});
const prompt = String(args._[0] ?? "").trim();
if (!prompt) {
  console.error('usage: bough prompt [-w dir] [-m model] [--yolo] [--json] "..."');
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
  const workspace = args.workspace
    ? await Deno.realPath(args.workspace)
    : Deno.cwd();
  const res = await post("/sessions", {
    title: "prompt: " + prompt.slice(0, 48),
    workspace,
    ...(args.model ? { model: args.model } : {}),
  });
  session = await res.json();
} catch (e) {
  console.error(`cannot reach bough server on :${port} — is it running? (${(e as Error).message})`);
  Deno.exit(2);
}

if (args.yolo) {
  await post("/net/yolo", { sessionId: session.id, on: true });
}

// Stream first, then send: the tail must be attached before the turn can end.
const events = await fetch(`${api}/events?sessionId=${session.id}`);
const reader = events.body!.pipeThrough(new TextDecoderStream()).getReader();

await post(`/sessions/${session.id}/messages`, { text: prompt });

let status = "timeout";
let usage: Record<string, unknown> = {};
let deadline = Date.now() + timeoutMs;
// usage.updated lands just AFTER turn.finished — take a short grace read for it.
let finishing = false;
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
    } else if (dataName === "usage.updated") {
      usage = data;
      if (finishing) break outer;
    } else if (dataName === "turn.finished") {
      status = String((data as { status?: string }).status ?? "done");
      if (!args.json) break outer;
      finishing = true;
      deadline = Math.min(deadline, Date.now() + 1500);
    }
  }
}
reader.cancel().catch(() => {});

if (args.json) {
  console.log(JSON.stringify({
    session: session.id,
    status,
    input_tokens: usage.inputTokens ?? null,
    output_tokens: usage.outputTokens ?? null,
    context_tokens: usage.contextTokens ?? null,
  }));
} else {
  // Deltas usually end mid-line; land the shell prompt cleanly.
  console.log();
}
Deno.exit(status === "done" || status === "stopped" ? 0 : 1);
