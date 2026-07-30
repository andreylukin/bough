/**
 * `bough exec [flags] "do the thing"` — the headless one-shot client.
 *
 * THE INVARIANT THIS FILE HOLDS, and the only reason it is worth a file of its
 * own: **the event stream is opened BEFORE the prompt is posted.** (spec §15,
 * plan §6.10.) The server answers `POST /sessions/:id/messages` with a 202 and
 * runs the turn behind it, reporting over `/events` — and `/events` has no
 * replay by design (`server/events.ts`: `seq` is a dedupe key, not a resume
 * cursor). A subscriber that attaches after the turn has already published
 * `turn.finished` will never see it, because there is nothing to catch up on and
 * nothing to ask for. The failure mode is not a dropped line of output: the CLI
 * waits out its full `--timeout` and exits 1 on a turn that actually succeeded,
 * and it only does that for turns fast enough to finish inside the post — which
 * is to say, for the cheapest and most-tested prompts, intermittently. That is
 * why the ordering is stated here, enforced by `runExec`, and pinned by a test
 * (`exec.test.ts`) whose fake server publishes `turn.finished` synchronously
 * inside the post handler: reverse the two calls and the event is published to
 * nobody and that test times out.
 *
 * Second invariant: **every effect is injected** (plan §0, DI over globals).
 * `runExec` takes a `fetch`, a stdout writer, a stderr writer, stdin, the
 * environment, and the cwd, and it RETURNS an exit code rather than calling
 * `process.exit`. The `import.meta.main` block at the bottom is the only code that
 * touches a real process. That is what lets the whole client be tested against
 * the real route table over an in-memory database, with no socket bound and
 * nothing on the network.
 *
 * Third: **argument parsing is pure and total.** `parseExecArgs` is a plain
 * function over a string array returning either arguments or a usage error; it
 * never reads the environment, never exits, and never throws. jsr.io is
 * unreachable in this environment, so there is no `@std/cli` here — and the flag
 * set is small enough that hand-parsing costs less than the dependency would.
 *
 * Exit codes are the contract with whatever shell or CI job wraps this:
 *
 *   0  the turn completed (`turn.finished` with status `done`)
 *   1  the turn did not complete — `error`, `interrupted`, `orphaned`, or the
 *      `--timeout` elapsed first
 *   2  usage problem (bad flag, no prompt) or connection problem (no server on
 *      the port, or it refused the session)
 *
 * Flags:
 *
 *   -w, --workspace DIR   the checkout the session operates on (default: cwd)
 *   -m, --model ID        pin the session's model
 *       --json            suppress streaming; print one result envelope
 *       --timeout SECS    give up after SECS (default 900); exits 1
 *       --port N          server port (default BOUGH_PORT, else 4321)
 *
 * Fourth: **a timeout STOPS the turn it abandoned.** `--timeout` elapsing used to
 * leave a turn running server-side, spending, with the next command against that
 * session queued behind it — the route to stop it did not exist. It does now
 * (`POST /sessions/:id/interrupt`, `server/turns.ts`), so the timeout path raises it
 * on a short deadline of its own and reports what actually happened rather than what
 * was intended. The exit code does not move: a turn this client gave up on did not
 * complete, whether or not the stop landed.
 */
import { realpath } from "node:fs/promises";
import type { Session, TurnStatus } from "../schema/parts.ts";
import type { MessageDeltaData, MessageRetryData, TurnFinishedData } from "../schema/events.ts";
import type { AskQuestion } from "../schema/parts.ts";
import type { UsageTotals } from "../types.ts";

// ---- arguments ---------------------------------------------------------------

/** The default wall clock for a whole turn. Generous: a real turn runs minutes. */
export const DEFAULT_TIMEOUT_SECONDS = 900;
/** Where the server is when neither `--port` nor `BOUGH_PORT` says otherwise. */
export const DEFAULT_PORT = 4321;

export const USAGE =
  'usage: bough exec [-w DIR] [-m MODEL] [--json] [--timeout SECS] [--port N] "prompt"\n' +
  "       (or pipe the prompt on stdin, with `-` or no positional argument)\n" +
  "\n" +
  "  -w, --workspace DIR   the checkout the turn runs in (default: cwd)\n" +
  "  -m, --model MODEL     override the model for this turn\n" +
  "      --json            one JSON envelope per line instead of streamed text\n" +
  "      --timeout SECS    wall clock for the whole turn (default: 900)\n" +
  "      --port N          server port (default: BOUGH_PORT, then 4321)\n" +
  "  -h, --help            this message\n" +
  "\n" +
  "programs run as you, with your authority — there is no sandbox.";

/** A well-formed invocation. `prompt` is still unresolved — `-` means "read stdin". */
export interface ExecArgs {
  /** The positional, verbatim. Empty or `-` defers to stdin. */
  prompt: string;
  workspace?: string;
  model?: string;
  json: boolean;
  /** Already in milliseconds, already validated positive and finite. */
  timeoutMs: number;
  /** Absent = fall back to `BOUGH_PORT`, then `DEFAULT_PORT`. */
  port?: number;
}

/** What `parseExecArgs` returns when the command line is not usable. Exit 2. */
export interface ExecUsageError {
  usageError: string;
}

/**
 * `--help` / `-h`. Distinct from a usage ERROR because it is not one: help was
 * asked for and help is being given, so it belongs on stdout with exit 0. Treating
 * it as an unknown flag — which is what happened before this existed — makes the
 * first thing anyone types at a new CLI print an error and fail a shell `&&`.
 */
export interface ExecHelpRequest {
  help: true;
}

export function isHelpRequest(
  x: ExecArgs | ExecUsageError | ExecHelpRequest,
): x is ExecHelpRequest {
  return "help" in x;
}

export function isUsageError(
  x: ExecArgs | ExecUsageError | ExecHelpRequest,
): x is ExecUsageError {
  return "usageError" in x;
}

const VALUE_FLAGS = new Set(["workspace", "model", "timeout", "port"]);
const SHORT: Record<string, string> = { w: "workspace", m: "model" };

/**
 * Pure, total argument parsing.
 *
 * Two decisions worth naming. **A second positional is an error, not something to
 * ignore.** `bough exec write the tests` is a forgotten pair of quotes, and taking
 * `args._[0]` would run the one-word prompt "write" and report success — the
 * failure would read as a bad model, not a bad command line. **Unknown flags are
 * errors too**, for the same reason: a typo'd `--jsno` that silently streams is
 * worse than one that stops.
 */
export function parseExecArgs(
  argv: readonly string[],
): ExecArgs | ExecUsageError | ExecHelpRequest {
  const positional: string[] = [];
  const values: Record<string, string> = {};
  let json = false;
  let onlyPositional = false;

  for (let i = 0; i < argv.length; i++) {
    const token = argv[i];
    if (onlyPositional) {
      positional.push(token);
      continue;
    }
    if (token === "--") {
      onlyPositional = true;
      continue;
    }

    let name: string | undefined;
    let inline: string | undefined;
    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      name = eq === -1 ? token.slice(2) : token.slice(2, eq);
      inline = eq === -1 ? undefined : token.slice(eq + 1);
    } else if (token.startsWith("-") && token.length > 1) {
      // A bare `-` is the stdin sentinel, not a flag — hence `length > 1`.
      const eq = token.indexOf("=");
      const short = eq === -1 ? token.slice(1) : token.slice(1, eq);
      if (short === "h") return { help: true };
      name = SHORT[short];
      if (!name) return { usageError: `unknown flag -${short}\n${USAGE}` };
      inline = eq === -1 ? undefined : token.slice(eq + 1);
    } else {
      positional.push(token);
      continue;
    }

    if (name === "help") return { help: true };
    if (name === "json") {
      if (inline !== undefined) return { usageError: `--json takes no value\n${USAGE}` };
      json = true;
      continue;
    }
    if (!VALUE_FLAGS.has(name)) return { usageError: `unknown flag --${name}\n${USAGE}` };
    if (inline !== undefined) {
      values[name] = inline;
      continue;
    }
    // Consume the next token even if it starts with `-`: a model id or a path may
    // legitimately do so, and refusing one here would be a rule the user cannot
    // work around.
    if (i + 1 >= argv.length) return { usageError: `--${name} needs a value\n${USAGE}` };
    values[name] = argv[++i];
  }

  if (positional.length > 1) {
    return {
      usageError:
        `expected one prompt, got ${positional.length} arguments — quote it as a single string\n${USAGE}`,
    };
  }

  let timeoutMs = DEFAULT_TIMEOUT_SECONDS * 1000;
  if (values.timeout !== undefined) {
    const seconds = Number(values.timeout);
    if (!Number.isFinite(seconds) || seconds <= 0) {
      return { usageError: `--timeout wants a positive number of seconds, got ${values.timeout}` };
    }
    timeoutMs = Math.round(seconds * 1000);
  }

  let port: number | undefined;
  if (values.port !== undefined) {
    port = Number(values.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return { usageError: `--port wants a port number, got ${values.port}` };
    }
  }

  return {
    prompt: positional[0] ?? "",
    ...(values.workspace !== undefined ? { workspace: values.workspace } : {}),
    ...(values.model !== undefined ? { model: values.model } : {}),
    json,
    timeoutMs,
    ...(port !== undefined ? { port } : {}),
  };
}

// ---- the SSE frame reader ------------------------------------------------------

/** One parsed SSE frame: the `event:` name and the decoded `data:` payload. */
export interface SseFrame {
  name: string;
  data: unknown;
}

/**
 * Incremental SSE parsing, split out so it is testable on strings.
 *
 * Frames are separated by a blank line and a chunk boundary can fall anywhere,
 * including mid-frame and mid-line, so nothing may be interpreted until its
 * terminator has arrived. Parsing per line — as the old tree did, tracking the
 * last `event:` seen across frames — quietly mislabels a payload whenever the
 * field order varies or a comment frame lands between the two lines.
 *
 * A frame whose `data:` is not JSON is dropped rather than thrown on: comment
 * frames (`: connected`, `: ping`) carry no data at all, and one malformed
 * payload must not end a turn that is otherwise streaming fine.
 */
export function createSseReader(): (chunk: string) => SseFrame[] {
  let buffer = "";
  return (chunk: string): SseFrame[] => {
    buffer += chunk.replace(/\r\n/g, "\n");
    const frames: SseFrame[] = [];
    let cut = buffer.indexOf("\n\n");
    while (cut !== -1) {
      const block = buffer.slice(0, cut);
      buffer = buffer.slice(cut + 2);
      const frame = parseSseBlock(block);
      if (frame) frames.push(frame);
      cut = buffer.indexOf("\n\n");
    }
    return frames;
  };
}

function parseSseBlock(block: string): SseFrame | undefined {
  let name = "message";
  const dataLines: string[] = [];
  for (const line of block.split("\n")) {
    if (line.startsWith(":")) continue; // comment — heartbeats land here
    if (line.startsWith("event:")) name = line.slice(6).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
  }
  if (dataLines.length === 0) return undefined;
  try {
    return { name, data: JSON.parse(dataLines.join("\n")) };
  } catch {
    return undefined;
  }
}

// ---- the run -------------------------------------------------------------------

/**
 * Everything `runExec` touches that is not a pure function. Production wires
 * these to the real process in `realDeps()`; a test wires them to a fake server
 * and a string buffer.
 */
export interface ExecDeps {
  /**
   * The call signature only, not `typeof fetch`: Bun's global carries a
   * `preconnect` property, and requiring it would make every test stub — a bare
   * arrow that answers or rejects — fail to typecheck for a method nothing here
   * calls.
   */
  fetchFn(input: string | URL | Request, init?: RequestInit): Promise<Response>;
  /** stdout. Assistant text goes here verbatim, and nothing else does. */
  write(text: string): void | Promise<void>;
  /** stderr. Diagnostics, retry notices, and usage errors — never the answer. */
  warn(text: string): void;
  /** The whole of piped stdin, decoded. Only called when the prompt defers to it. */
  readStdin(): Promise<string>;
  stdinIsTerminal(): boolean;
  env(name: string): string | undefined;
  cwd(): string;
  /** Resolves and validates the `--workspace` directory. Throws if it is not one. */
  realPath(path: string): Promise<string>;
}

/** The `--json` result envelope. One line, printed once, on a finished turn. */
export interface ExecEnvelope {
  session: string;
  status: TurnStatus | "timeout";
  /** The exit code this envelope corresponds to being 0. */
  ok: boolean;
  /** The assistant text `--json` suppressed from stdout. Not dropped, relocated. */
  text: string;
  /** Present when the turn errored — the server's own message. */
  error?: string;
  /** Absent if the post-turn fetch failed; the envelope is still printed. */
  usage?: UsageTotals;
  /** This session plus every subagent and workflow agent collapsed under it. */
  treeUsage?: UsageTotals;
}

/**
 * The whole client. Returns the process exit code; never exits, never writes to a
 * real stream, never reads a global.
 */
export async function runExec(argv: readonly string[], deps: ExecDeps): Promise<number> {
  const parsed = parseExecArgs(argv);
  if (isHelpRequest(parsed)) {
    deps.write(`${USAGE}\n`);
    return 0;
  }
  if (isUsageError(parsed)) {
    deps.warn(parsed.usageError);
    return 2;
  }

  // The prompt: the positional, or stdin when it is `-` or absent with stdin
  // piped. An absent positional on a TERMINAL is the empty invocation — `bough
  // exec` alone — and reading stdin there would hang on the user's keyboard with
  // no prompt shown, so it is a usage error instead.
  let prompt = parsed.prompt.trim();
  if (prompt === "-" || (prompt === "" && !deps.stdinIsTerminal())) {
    try {
      prompt = (await deps.readStdin()).trim();
    } catch (e) {
      deps.warn(`cannot read the prompt from stdin: ${messageOf(e)}`);
      return 2;
    }
  }
  if (!prompt) {
    deps.warn(USAGE);
    return 2;
  }

  const port = parsed.port ?? Number(deps.env("BOUGH_PORT") ?? DEFAULT_PORT);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    deps.warn(`BOUGH_PORT is not a port number: ${deps.env("BOUGH_PORT")}`);
    return 2;
  }
  const api = `http://127.0.0.1:${port}`;

  // One deadline over the whole run, not just the wait: a server that accepts the
  // connection and then never answers must not hang forever either. `timedOut`
  // distinguishes "the deadline fired" from "the socket died", which is the
  // difference between exit 1 and exit 2.
  let timedOut = false;
  const deadline = new AbortController();
  const timer = setTimeout(() => {
    timedOut = true;
    deadline.abort();
  }, parsed.timeoutMs);

  try {
    let workspace: string;
    try {
      workspace = parsed.workspace ? await deps.realPath(parsed.workspace) : deps.cwd();
    } catch (e) {
      deps.warn(`--workspace ${parsed.workspace}: ${messageOf(e)}`);
      return 2;
    }

    // 1. The session.
    let session: Session;
    try {
      const res = await deps.fetchFn(`${api}/sessions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          title: `exec: ${prompt.slice(0, 48)}`,
          workspace,
          ...(parsed.model ? { model: parsed.model } : {}),
        }),
        signal: deadline.signal,
      });
      if (!res.ok) {
        deps.warn(`bough refused the session: ${res.status} ${(await res.text()).trim()}`);
        return 2;
      }
      session = await res.json() as Session;
    } catch (e) {
      deps.warn(
        timedOut
          ? `timed out connecting to bough on :${port}`
          : `cannot reach bough on :${port} — is the server running? (${messageOf(e)})`,
      );
      return 2;
    }

    // 2. THE ORDERING. The stream is opened, and its bus subscription is live,
    //    before the prompt exists server-side. Everything about this file is
    //    arranged so these two statements cannot be swapped by accident: the post
    //    below reads `reader`, so moving it up is a compile error, not a bug.
    let reader: ReadableStreamDefaultReader<string>;
    try {
      const events = await deps.fetchFn(`${api}/events?sessionId=${session.id}`, {
        signal: deadline.signal,
      });
      if (!events.ok || !events.body) {
        deps.warn(`bough refused the event stream: ${events.status}`);
        return 2;
      }
      reader = events.body.pipeThrough(new TextDecoderStream()).getReader();
    } catch (e) {
      deps.warn(
        timedOut
          ? `timed out opening the event stream on :${port}`
          : `cannot open the bough event stream on :${port} (${messageOf(e)})`,
      );
      return 2;
    }

    // 3. The prompt. A turn that finishes inside this call is already in the
    //    stream's queue by the time we read it — that is the point of step 2.
    try {
      const res = await deps.fetchFn(`${api}/sessions/${session.id}/messages`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ text: prompt }),
        signal: deadline.signal,
      });
      if (!res.ok) {
        deps.warn(`bough refused the message: ${res.status} ${(await res.text()).trim()}`);
        await cancel(reader);
        return 2;
      }
    } catch (e) {
      deps.warn(
        timedOut
          ? `timed out posting the prompt to :${port}`
          : `cannot post the prompt to bough on :${port} (${messageOf(e)})`,
      );
      await cancel(reader);
      return 2;
    }

    // 4. Consume until the turn ends.
    const feed = createSseReader();
    let status: TurnStatus | "timeout" = "timeout";
    let error: string | undefined;
    let text = "";
    let streamed = false;

    outer: for (;;) {
      let chunk: Awaited<ReturnType<typeof reader.read>>;
      try {
        chunk = await reader.read();
      } catch {
        // The deadline aborted the body, or the connection died mid-turn. Both
        // leave `status` at its default and are reported below.
        break;
      }
      if (chunk.done || chunk.value === undefined) break;
      for (const frame of feed(chunk.value)) {
        const data = payloadOf(frame.data);
        switch (frame.name) {
          case "message.delta": {
            const delta = (data as MessageDeltaData | undefined)?.delta;
            if (!delta) break;
            text += delta;
            if (!parsed.json) {
              await deps.write(delta);
              streamed = true;
            }
            break;
          }
          case "message.retry": {
            // The message re-streams from the top (schema/events.ts), so whatever
            // reached stdout is about to be repeated. stdout cannot be un-written,
            // so the boundary is announced on stderr and the captured text is
            // dropped — the envelope must carry the answer, not the false start.
            const retry = data as MessageRetryData | undefined;
            text = "";
            deps.warn(
              `[retry ${retry?.attempt ?? "?"}: ${retry?.reason ?? "no reason given"}]`,
            );
            break;
          }
          case "ask.question": {
            // NOBODY IS HERE TO ANSWER. A program that calls `ask()` — or a workflow
            // launch, which raises an approval card by default (`workflow/control.ts`) —
            // parked forever under this client: exec had no case for the event, so the
            // turn sat held until `--timeout` and exited 1 on work that was one answer
            // from finishing. Declining is the documented dismissal (spec §6: `ask()`
            // throws a catchable "user declined"), so the program gets an error it can
            // act on and the turn ends on its own terms.
            const held = data as AskQuestion | undefined;
            if (!held || held.status !== "pending") break;
            deps.warn(
              `[declined a question — bough exec is not interactive: ${
                held.question.split("\n")[0].slice(0, 120)
              }]`,
            );
            // Fire and forget: a failed decline leaves the old behaviour (a hold that
            // waits out the deadline), and blocking the event loop on it would be worse.
            void deps.fetchFn(
              `${api}/sessions/${encodeURIComponent(held.sessionId)}/questions/${
                encodeURIComponent(held.id)
              }`,
              {
                method: "POST",
                headers: { "content-type": "application/json" },
                body: JSON.stringify({ decline: true }),
              },
            ).catch(() => {});
            break;
          }
          case "turn.finished": {
            const finished = data as TurnFinishedData | undefined;
            status = finished?.status ?? "done";
            error = finished?.error;
            break outer;
          }
        }
      }
    }
    await cancel(reader);

    if (status === "timeout") {
      // Stop what we walked away from. On its OWN deadline, because the run's
      // deadline has already fired by definition and its signal is aborted — reusing
      // it would abort this request before it was sent. Best-effort in both
      // directions: a stop that fails is reported and changes nothing else, since the
      // turn is unfinished either way.
      const stopped = await stopTurn(api, session.id, deps);
      deps.warn(
        timedOut
          ? `timed out after ${parsed.timeoutMs / 1000}s — ${
            stopped ? "interrupted" : "could NOT interrupt"
          } the turn in session ${session.id}`
          : `the event stream closed before the turn finished — ${
            stopped ? "interrupted" : "could NOT interrupt"
          } the turn in session ${session.id}`,
      );
    } else if (error) {
      deps.warn(`turn ${status}: ${error}`);
    }

    const ok = status === "done";

    if (parsed.json) {
      // Usage comes from `GET /sessions/:id` after the turn, not from the stream:
      // it is the reconnect endpoint and therefore the authoritative record, and
      // the cache splits that decide the cost are only summed once the turn ends.
      // Best-effort — an envelope without usage still tells the caller what
      // happened, and a failed metrics fetch must not change the exit code.
      const envelope: ExecEnvelope = { session: session.id, status, ok, text };
      if (error) envelope.error = error;
      try {
        const res = await deps.fetchFn(`${api}/sessions/${session.id}`, {
          signal: deadline.signal,
        });
        if (res.ok) {
          const body = await res.json() as { usage?: UsageTotals & { tree?: UsageTotals } };
          if (body.usage) {
            const { tree, ...totals } = body.usage;
            envelope.usage = totals;
            if (tree) envelope.treeUsage = tree;
          }
        } else {
          await res.body?.cancel();
        }
      } catch {
        // Reported by its absence from the envelope.
      }
      await deps.write(JSON.stringify(envelope) + "\n");
    } else if (streamed) {
      // Deltas end mid-line far more often than not; land the shell prompt cleanly.
      await deps.write("\n");
    }

    return ok ? 0 : 1;
  } finally {
    clearTimeout(timer);
  }
}

/** How long the abandon-time interrupt gets. Short: nobody is waiting on the answer. */
const INTERRUPT_TIMEOUT_MS = 5_000;

/**
 * Raise the user interrupt on a turn this client is giving up on (spec §5).
 *
 * Returns whether a turn was actually signalled. `false` covers three different
 * things — the request failed, the server said nothing was running, the deadline
 * elapsed — and the caller deliberately does not distinguish them: all three mean
 * "do not claim it was stopped", which is the only claim worth being careful about.
 */
async function stopTurn(api: string, sessionId: string, deps: ExecDeps): Promise<boolean> {
  const stop = new AbortController();
  const timer = setTimeout(() => stop.abort(), INTERRUPT_TIMEOUT_MS);
  try {
    const res = await deps.fetchFn(`${api}/sessions/${sessionId}/interrupt`, {
      method: "POST",
      signal: stop.signal,
    });
    if (!res.ok) {
      await res.body?.cancel();
      return false;
    }
    const body = await res.json() as { interrupted?: boolean };
    return body.interrupted === true;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * The event payload sits at `envelope.data` — the SSE frame carries the whole
 * stamped `BoughEvent`, not the bare payload.
 */
function payloadOf(envelope: unknown): unknown {
  if (envelope && typeof envelope === "object" && "data" in envelope) {
    return (envelope as { data: unknown }).data;
  }
  return undefined;
}

/** Releasing the stream is best-effort: the process is about to exit regardless. */
async function cancel(reader: ReadableStreamDefaultReader<string>): Promise<void> {
  try {
    await reader.cancel();
  } catch {
    // Already errored or already cancelled.
  }
}

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// ---- the process -----------------------------------------------------------------

/** The real process, wired up once. The only impure thing in this file. */
export function realDeps(): ExecDeps {
  return {
    fetchFn: (input, init) => fetch(input, init),
    // `Bun.write` resolves only once the whole string is out, so the partial-write
    // loop a raw fd write needs is not needed here.
    write: async (text) => {
      await Bun.write(Bun.stdout, text);
    },
    warn: (text) => console.error(text),
    readStdin: () => Bun.stdin.text(),
    stdinIsTerminal: () => process.stdin.isTTY === true,
    env: (name) => process.env[name],
    cwd: () => process.cwd(),
    realPath: (path) => realpath(path),
  };
}

if (import.meta.main) {
  process.exit(await runExec(process.argv.slice(2), realDeps()));
}
