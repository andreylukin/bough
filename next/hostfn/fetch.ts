/**
 * `fetch(url, opts)` — the program's structured HTTP verb.
 *
 * THE INVARIANT THIS HOLDS: **a response is DATA; only the absence of a response is
 * an error.** A 404, a 500, a 403 — every one of them comes back as
 * `{status, ok, body, …}` for the program to branch on. Exactly four things reject: a
 * URL that is not http(s), a transport failure (DNS, connection refused, TLS), the
 * 30s deadline, and the turn's interrupt. That split is the whole design, and getting
 * it backwards is a real failure mode both ways: a thrown 404 turns "the endpoint
 * says not found, which answers my question" into a dead round, and a swallowed
 * timeout turns "nothing came back" into an empty body the model reports as the truth
 * about the page.
 *
 * The three limits exist so one call cannot wreck a turn:
 *
 *   - **1MB body cap, with `truncated: true` saying so.** Enforced by reading the
 *     stream chunk by chunk and stopping, not by `res.text()` — the whole point is
 *     never to buffer the multi-GB download host-side in order to discover it was
 *     multi-GB. And the flag is not cosmetic: a program handed a silently cut JSON
 *     document parses garbage and reports it confidently.
 *   - **A 30s deadline.** The program cap covers the whole round; one stalled request
 *     must not spend it.
 *   - **The turn's interrupt.** An interrupt tears down the worker, which would
 *     otherwise leave the request in flight with nobody to read it.
 *
 * The deadline and the interrupt fold into one `AbortController`, and the catch tells
 * them apart by the abort *reason* — because "the server took more than 30 seconds"
 * and "the user stopped you" call for different next moves, and an `AbortError`
 * carries no text that says which happened.
 *
 * NO EGRESS WALL, STATED PLAINLY. Whatever URL the program passes leaves the machine
 * verbatim, with the user's identity and the machine's credentials; there is no proxy
 * and no filter (spec §2, §17). Hence the deliberate narrowness — http/https only, so
 * `file:` and `data:` cannot read the host through a URL — and no credential
 * injection: headers are only what the program spelled out.
 *
 * Ported from `src/tools/fetch_url.ts`. Deltas are marked `NOTE:`.
 */
import { z } from "zod";
import { NetError } from "../errors.ts";
import type { HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/** Body cap. A bigger response comes back cut, with `truncated: true`. */
export const MAX_BYTES = 1_000_000;

/** One request must never stall a turn; the program cap is not a deadline. */
export const DEADLINE_MS = 30_000;

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * The options a program may pass, validated at the boundary (plan §0).
 *
 * Unknown keys are rejected rather than ignored: a model that writes `{timeout: 5}`
 * expecting it to be honored has to be told it is not, or it will read the 30s
 * deadline as a bug in its own program.
 */
export const FetchOptions = z.object({
  method: z.string().min(1).optional(),
  headers: z.record(z.string(), z.string()).optional(),
  body: z.string().optional(),
}).strict();
export type FetchOptions = z.infer<typeof FetchOptions>;

export interface FetchResult {
  status: number;
  ok: boolean;
  /** The FINAL url — redirects are followed, so this may differ from the request. */
  url: string;
  contentType: string;
  body: string;
  /** True when the body hit `MAX_BYTES` and what came back is a prefix. */
  truncated: boolean;
}

/** The seams. All three default to production behavior. */
export interface FetchDeps {
  /** Injected so tests need no socket and no network. Absent = `globalThis.fetch`. */
  fetchImpl?: typeof globalThis.fetch;
  maxBytes?: number;
  deadlineMs?: number;
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/**
 * One HTTP request from the host. Resolves for ANY response the server sent;
 * rejects only for a bad URL, a transport failure, the deadline, or the interrupt.
 */
export async function fetchUrl(
  url: string,
  opts: FetchOptions = {},
  signal?: AbortSignal,
  deps: FetchDeps = {},
): Promise<FetchResult> {
  const maxBytes = deps.maxBytes ?? MAX_BYTES;
  const deadlineMs = deps.deadlineMs ?? DEADLINE_MS;
  const impl = deps.fetchImpl ?? globalThis.fetch;

  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new NetError(
      400,
      `fetch: ${JSON.stringify(url)} is not a valid URL. Pass an absolute ` +
        `http:// or https:// URL, host included.`,
    );
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new NetError(
      400,
      `fetch: only http and https URLs are allowed — got ${parsed.protocol} in ` +
        `${url}. Read local files with Deno.readTextFile or bash instead.`,
    );
  }

  // Two abort sources folded into the one signal `fetch` takes. The timer is cleared
  // on every exit path, so a finished call cannot keep the process alive.
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(DEADLINE_REASON), deadlineMs);
  const onInterrupt = () => controller.abort(INTERRUPT_REASON);
  if (signal?.aborted) controller.abort(INTERRUPT_REASON);
  signal?.addEventListener("abort", onInterrupt, { once: true });

  try {
    const res = await impl(parsed, {
      method: opts.method ?? "GET",
      ...(opts.headers ? { headers: opts.headers } : {}),
      ...(opts.body === undefined ? {} : { body: opts.body }),
      signal: controller.signal,
      // Followed, and the FINAL url is reported back — a program that resolved a
      // short link needs to know where it landed.
      redirect: "follow",
    });
    const { text, truncated } = await readCapped(res.body, maxBytes);
    return {
      status: res.status,
      ok: res.ok,
      url: res.url || parsed.href,
      contentType: res.headers.get("content-type") ?? "",
      body: text,
      truncated,
    };
  } catch (err) {
    throw new NetError(502, `fetch ${parsed.href} failed: ${failureReason(controller, err)}`);
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", onInterrupt);
  }
}

const DEADLINE_REASON = "deadline";
const INTERRUPT_REASON = "interrupted";

/**
 * Why the request produced no response, in words that pick the next move.
 *
 * An `AbortError` says nothing about which abort fired, so the controller's own
 * reason is the authority: a deadline is worth retrying or narrowing, an interrupt is
 * the user saying stop, and anything else is a transport fault to report rather than
 * retry blindly.
 */
function failureReason(controller: AbortController, err: unknown): string {
  if (controller.signal.aborted) {
    const reason = controller.signal.reason;
    if (reason === INTERRUPT_REASON) {
      return "the turn was interrupted before the response arrived";
    }
    if (reason === DEADLINE_REASON) {
      return `no response within ${DEADLINE_MS / 1000}s (the request was cancelled)`;
    }
  }
  return (err as Error)?.message ?? String(err);
}

/**
 * Read at most `maxBytes` of a body, then stop and cancel the rest.
 *
 * `truncated` means bytes were DROPPED, not "the buffer filled up". A body of exactly
 * `maxBytes` is whole, and flagging it would be a lie in the direction that costs
 * something: a model told its complete JSON document was cut re-fetches it in ranges,
 * or hedges an answer it actually had. So a full buffer is confirmed against one more
 * read before the flag is set.
 *
 * NOTE: the cap is on BYTES read, not characters decoded, and the decode happens once
 * over the accumulated buffer rather than per chunk — a multi-byte character split
 * across a chunk boundary would otherwise decode as replacement characters in the
 * middle of otherwise fine text.
 */
async function readCapped(
  body: ReadableStream<Uint8Array> | null,
  maxBytes: number,
): Promise<{ text: string; truncated: boolean }> {
  if (!body) return { text: "", truncated: false };
  const buf = new Uint8Array(maxBytes);
  let filled = 0;
  let truncated = false;
  const reader = body.getReader();
  try {
    while (filled < maxBytes) {
      const { done, value } = await reader.read();
      if (done) break;
      const room = maxBytes - filled;
      if (value.length > room) {
        // This chunk alone overflows: keep what fits, drop the rest of the download
        // rather than buffering a multi-GB body host-side to discover its size.
        buf.set(value.subarray(0, room), filled);
        filled = maxBytes;
        truncated = true;
        await reader.cancel();
        break;
      }
      buf.set(value, filled);
      filled += value.length;
    }
    // The buffer filled exactly. One more read says whether anything was left.
    if (!truncated && filled === maxBytes) {
      const { done } = await reader.read();
      if (!done) {
        truncated = true;
        await reader.cancel();
      }
    }
  } finally {
    reader.releaseLock();
  }
  return { text: new TextDecoder().decode(buf.subarray(0, filled)), truncated };
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/**
 * Build the bridged `fetch` host function for one turn.
 *
 * The turn's `signal` is wired in here rather than passed by the program, which is
 * what makes an interrupt reach a request already in flight — the program cannot
 * cancel what it is parked on.
 */
export function createFetchHostFn(ctx: TurnCtx, deps: FetchDeps = {}): Pick<HostFns, "fetch"> {
  return {
    fetch: async (url: string, optsJson: string): Promise<string> => {
      let raw: unknown;
      try {
        raw = optsJson === "" ? {} : JSON.parse(optsJson);
      } catch {
        throw new NetError(400, "fetch: the options argument was not valid JSON");
      }
      const parsed = FetchOptions.safeParse(raw ?? {});
      if (!parsed.success) {
        throw new NetError(
          400,
          `fetch: invalid options: ${
            parsed.error.issues
              .map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`)
              .join("; ")
          }. It takes {method?, headers?, body?} and nothing else — the 1MB cap and ` +
            `the 30s deadline are not configurable.`,
        );
      }
      return JSON.stringify(await fetchUrl(url, parsed.data, ctx.signal, deps));
    },
  };
}
