/**
 * The host side of the program worker: one `run_steps` program per round, executed
 * in a fresh `Worker` with `permissions: "inherit"`.
 *
 * WHY THIS EXISTS. bough's thesis is one program per round (spec §1), and a program
 * runs as the user with the user's full authority — filesystem, network, env,
 * subprocesses, `npm:`/`jsr:` imports. **Nothing here is a security boundary**
 * (spec §2.2). The bridged host functions are convenience and session integration,
 * never confinement: `bash` carries the turn's interrupt and the 60s
 * auto-background, `view`/`patch` carry the hash anchoring that makes concurrent
 * subagents safe, `agent`/`ask`/`state` reach the session's database and UI. A
 * program is free to ignore every one of them and call `Deno` directly.
 *
 * THE INVARIANT THIS HOLDS: **a program never outlives its turn, and never takes
 * the server with it.** Three mechanisms, each of which exists because the isolate
 * is *not* sealed:
 *
 *   1. **Pre-flight, before a worker is spawned.** A program that cannot compile
 *      used to reach the model as a bare `SyntaxError` over ten frames of Deno
 *      internals — no line, no column, no source — and the model burned the round
 *      guessing. `checkProgramSyntax` parses with the *same* parameter list the
 *      worker binds, so the two can never disagree about what is legal. That is why
 *      shadowing a host name (`let bash = 1`) is caught here and reported as a
 *      shadow, not discovered inside the worker as a mystery.
 *   2. **Wind-down is a handshake, not a `terminate()`.** A program spawns children
 *      of the SERVER process, and `worker.terminate()` does not touch them. So an
 *      abort or a wall-clock timeout asks the worker to kill what it spawned, waits
 *      briefly for the ack, and only then terminates. Reverse order orphans
 *      processes — ^C would look like it worked while the build kept running
 *      (plan §6.3).
 *   3. **Partial output survives.** `console.*` lines are streamed as they are
 *      printed *and* batched into the result. An interrupt terminates the worker
 *      before it can post its batch, so the streamed copy kept here is what keeps
 *      the model's tool result non-empty.
 *
 * Host names come from `protocol.ts`, imported by both sides — see that module's
 * header for why there is exactly one list. This file adds no name of its own.
 *
 * Ported from `src/harness/vm.ts`. Deltas from that port are marked `NOTE:`.
 */

import {
  type FromProgramWorker,
  HOST_FN_NAMES,
  type HostCallMessage,
  type HostFnName,
  PROGRAM_PARAMS,
  type ProgramResult,
} from "./protocol.ts";
import type { HostFns } from "../types.ts";

/**
 * The wall-clock ceiling on one program. Not a resource limit — a liveness one: a
 * program wedged in a synchronous loop or blocked on a host call that will never
 * answer would otherwise hang the turn forever.
 */
export const DEFAULT_TIMEOUT_MS = 180_000;

/**
 * How long a stopping program gets to kill the processes it spawned before the
 * worker is terminated regardless. Long enough for a SIGTERM sweep, short enough
 * that the stop button still feels instant. A worker wedged in a synchronous loop
 * cannot answer at all, which is exactly why this is a timeout and not a wait.
 */
export const ABORT_GRACE_MS = 1_000;

// ---------------------------------------------------------------------------
// Pre-flight
// ---------------------------------------------------------------------------

// deno-lint-ignore no-explicit-any
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

/**
 * Locate a string literal closed by a raw newline.
 *
 * This is THE failure mode for model-generated code: the model assembles the
 * program inside a template literal, and every `\n` meant for a string in the
 * GENERATED program is consumed by the outer literal, leaving a real newline inside
 * `"..."`. V8 reports it as "Invalid or unexpected token" with no position, which
 * tells the author nothing.
 *
 * A scanner rather than a regex because it has to skip comments and template
 * literals, where a raw newline is perfectly legal.
 *
 * NOTE: ported from `src/text.ts`, which the new layout does not carry forward.
 * It lives here because its only caller is the pre-flight check, and because the
 * check must parse with the worker's own parameter list to be meaningful.
 */
export function unterminatedString(
  src: string,
): { line: number; col: number; text: string; quote: string } | null {
  let line = 1, col = 1, depth = 0;
  for (let i = 0; i < src.length; i++) {
    const c = src[i], next = src[i + 1];
    if (c === "/" && next === "/") {
      while (i < src.length && src[i] !== "\n") i++;
      line++;
      col = 1;
      continue;
    }
    if (c === "/" && next === "*") {
      const end = src.indexOf("*/", i + 2);
      const skipped = src.slice(i, end < 0 ? src.length : end + 2);
      line += (skipped.match(/\n/g) ?? []).length;
      col = 1;
      i = end < 0 ? src.length : end + 1;
      continue;
    }
    // Template literals may span lines legally; walk them (with `${}` nesting) so
    // their newlines never look like an unterminated quote.
    if (c === "`") {
      i++;
      for (; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n") {
          line++;
          col = 1;
          continue;
        }
        if (src[i] === "$" && src[i + 1] === "{") depth++;
        else if (src[i] === "}" && depth > 0) depth--;
        else if (src[i] === "`" && depth === 0) break;
      }
      col++;
      continue;
    }
    if (c === '"' || c === "'") {
      const startLine = line, startCol = col;
      for (i++; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n" || (i === src.length - 1 && src[i] !== c)) {
          return {
            line: startLine,
            col: startCol,
            text: src.split("\n")[startLine - 1] ?? "",
            quote: c,
          };
        }
        if (src[i] === c) break;
      }
      col++;
      continue;
    }
    if (c === "\n") {
      line++;
      col = 1;
    } else col++;
  }
  return null;
}

/**
 * Compile-check a program BEFORE spawning a worker. Returns the message to hand the
 * model, or `null` when the program parses.
 *
 * Compiling is side-effect free: the `AsyncFunction` constructor parses, it does not
 * execute, and the code never touches this scope. The parameter list is
 * `PROGRAM_PARAMS` — the same list `vm_worker.ts` binds — so a program that shadows
 * a host name is a `SyntaxError` in both places, and this one is the one that can
 * explain itself.
 */
export function checkProgramSyntax(code: string): string | null {
  try {
    new AsyncFunction(...PROGRAM_PARAMS, code);
    return null;
  } catch (err) {
    if ((err as Error)?.name !== "SyntaxError") throw err;
    const why = (err as Error).message;
    // NOTE: the shadow case is worth naming outright. V8 says "Identifier 'bash'
    // has already been declared", which reads as a bug in the program's own scope
    // unless you know `bash` arrived as a parameter.
    const shadow = /Identifier '([^']+)' has already been declared/.exec(why);
    if (shadow && (PROGRAM_PARAMS as readonly string[]).includes(shadow[1])) {
      return `program does not parse: ${why} — \`${shadow[1]}\` is a host function ` +
        `already bound in every program's scope, so declaring it shadows the binding. ` +
        `Rename your variable (\`my${shadow[1][0].toUpperCase()}${shadow[1].slice(1)}\`) ` +
        `and call \`${shadow[1]}\` as it is.`;
    }
    const hit = unterminatedString(code);
    if (!hit) return `program does not parse: ${why}`;
    return `program does not parse: ${why} — line ${hit.line}: a ${
      hit.quote === '"' ? "double" : "single"
    }-quoted string is closed by a real newline.\n  ${hit.line} | ${
      clip(hit.text.trim(), 90)
    }\nIf you built this code inside a template literal, write \\\\n (escaped) for ` +
      `newlines that belong to the GENERATED code's strings — a bare \\n is consumed ` +
      `by the outer literal.`;
  }
}

// ---------------------------------------------------------------------------
// Running one program
// ---------------------------------------------------------------------------

/**
 * NOTE: an options object where the port took five positional arguments. The
 * caller (`turn/runner.ts`) passes `signal` and `onLog` but rarely `timeoutMs`, and
 * a bare `runProgram(code, host, undefined, signal)` is exactly the shape that
 * grows a bug when a parameter is inserted.
 */
export interface RunProgramOptions {
  /** The program source, as the model wrote it. */
  code: string;
  /**
   * The bridged host functions. **Absence is the capability denial** — a name the
   * turn does not bridge simply is not here, and calling it rejects catchably
   * (types.ts `HostFns`).
   */
  host: HostFns;
  /** Wall-clock ceiling. Default `DEFAULT_TIMEOUT_MS`. */
  timeoutMs?: number;
  /** The turn's interrupt. Winds the program down: children first, then the worker. */
  signal?: AbortSignal;
  /**
   * Fires for each `console.*` line as the program prints it. Display-only — the
   * batched `logs` in the result carry the same lines regardless (spec §5).
   */
  onLog?: (line: string) => void;
}

/**
 * Run one program to completion, a timeout, or an interrupt, and resolve with what
 * the model should see. **This never rejects**: a program that throws, times out,
 * or is interrupted is an ordinary result with `ok: false`, because every one of
 * those is something the next round can act on.
 */
export function runProgram(opts: RunProgramOptions): Promise<ProgramResult> {
  const { code, host, timeoutMs = DEFAULT_TIMEOUT_MS, signal, onLog } = opts;

  // Parse before spawning. The worker parses it again for real; this pass exists
  // only to say WHERE, and to cost a fast round-trip instead of a worker spawn
  // (spec §6).
  const bad = checkProgramSyntax(code);
  if (bad) return Promise.resolve({ ok: false, logs: [], error: bad });

  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
    // The program runs with everything the server itself has. The host functions
    // are convenience and integration, NOT a boundary, and a program is free to
    // reach past them to raw Deno APIs (spec §2.2).
    deno: { permissions: "inherit" },
  });

  return new Promise<ProgramResult>((resolve) => {
    let settled = false;
    /** Set while a stop is in flight; the worker's `aborted` ack calls it. */
    let onAborted: (() => void) | undefined;
    let grace: ReturnType<typeof setTimeout> | undefined;
    /**
     * Console lines already streamed out of the worker. An interrupt terminates the
     * worker before it can post its batched `logs`, so this copy is what keeps the
     * partial output in the tool result.
     */
    const streamed: string[] = [];

    /** Spec §6: a timeout and an interrupt must be distinguishable, and each must
     * say what partial work survived. "failed" alone is a defect. */
    const survived = () =>
      streamed.length === 0
        ? "it printed nothing before stopping; anything it had already done (files written, commands run) still stands"
        : `the ${streamed.length} line(s) it printed before stopping are above; ` +
          `anything it had already done (files written, commands run) still stands`;

    const interrupted = (): ProgramResult => ({
      ok: false,
      logs: streamed,
      interrupted: true,
      error: `program interrupted by the user — ${survived()}`,
    });
    const timedOut = (): ProgramResult => ({
      ok: false,
      logs: streamed,
      error: `program timed out after ${timeoutMs}ms — ${survived()}. ` +
        `Long-running commands belong in bashBg(), not in a foreground wait.`,
    });

    const finish = (result: ProgramResult) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      // A stop already in flight leaves its grace timer armed; an unclaimed timer
      // keeps the process (and Deno's test sanitizer) awake for another second.
      if (grace !== undefined) clearTimeout(grace);
      signal?.removeEventListener("abort", onAbort);
      worker.terminate();
      resolve(result);
    };

    /**
     * Stopping is a handshake. The program runs with real permissions, so it may
     * have spawned processes of its own — children of THIS server process, which
     * `worker.terminate()` leaves running forever. So: ask the worker to kill what
     * it spawned, wait briefly for the ack, then terminate. A worker wedged in a
     * synchronous loop cannot answer, hence the grace timeout — the turn stops on
     * schedule either way (plan §6.3).
     */
    const stop = (result: ProgramResult) => {
      if (settled || onAborted) return;
      const done = () => {
        if (grace !== undefined) clearTimeout(grace);
        finish(result);
      };
      onAborted = done;
      grace = setTimeout(done, ABORT_GRACE_MS);
      try {
        worker.postMessage({ type: "abort" });
      } catch {
        done(); // worker already gone — nothing to wind down
      }
    };
    const onAbort = () => stop(interrupted());

    // A timed-out program gets the same wind-down as an interrupted one: whatever
    // it spawned is killed before the worker goes away.
    const timer = setTimeout(() => stop(timedOut()), timeoutMs);

    worker.onmessage = async (e: MessageEvent) => {
      const msg = e.data as FromProgramWorker;
      // The worker finished killing what it spawned — stop waiting on the grace timer.
      if (msg.type === "aborted") return onAborted?.();
      if (msg.type === "log") {
        streamed.push(msg.line);
        onLog?.(msg.line);
        return;
      }
      if (msg.type === "done") return finish({ ok: true, logs: msg.logs });
      if (msg.type === "error") return finish({ ok: false, logs: msg.logs, error: msg.message });
      await hostCall(msg);
    };

    /** One bridged call: run it here, post the result — or the error — back in. */
    const hostCall = async (msg: HostCallMessage) => {
      try {
        // Validate against the canonical list before indexing: the worker global is
        // reachable from the program, so `fn` is not guaranteed to be one of ours,
        // and `host["constructor"]` would otherwise be "callable".
        if (!(HOST_FN_NAMES as readonly string[]).includes(msg.fn)) {
          throw new Error(`unknown host function: ${msg.fn}`);
        }
        const fn = host[msg.fn as HostFnName];
        if (typeof fn !== "function") {
          // Absence is the capability denial (types.ts `HostFns`). Say which, and
          // that the prompt is the authority — the model must not retry blind.
          throw new Error(
            `${msg.fn}() is not available in this turn — the system prompt lists the ` +
              `host functions this session was granted. Use another approach.`,
          );
        }
        // deno-lint-ignore no-explicit-any
        const value = await (fn as any).apply(host, msg.args);
        post({ type: "host_result", id: msg.id, ok: true, value: String(value) });
      } catch (err) {
        post({
          type: "host_result",
          id: msg.id,
          ok: false,
          value: (err as Error)?.message ?? String(err),
        });
      }
    };

    const post = (m: unknown) => {
      // The worker may have been terminated (interrupt) while a host call was in
      // flight; posting into a dead worker is not an error worth surfacing.
      if (settled) return;
      try {
        worker.postMessage(m);
      } catch { /* worker gone */ }
    };

    worker.onerror = (e) => {
      e.preventDefault();
      finish({ ok: false, logs: streamed, error: `worker error: ${e.message}` });
    };

    // Already stopped before we started: the program never ran, so there is nothing
    // to wind down and no reason to wait on an ack.
    if (signal?.aborted) return finish(interrupted());
    signal?.addEventListener("abort", onAbort, { once: true });

    worker.postMessage({ type: "run", code });
  });
}
