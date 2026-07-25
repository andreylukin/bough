/**
 * fetch(): the program's ONLY way to reach the network. The sandbox worker runs with
 * `permissions: "none"`, so it has no fetch of its own; this runs here on the host,
 * where the server process holds full network access. Shelling `curl` through bash()
 * works too, but it is a subprocess per request, its errors arrive as exit codes, and
 * the model has to parse headers out of text — this returns one structured value.
 *
 * There is no egress wall behind this: whatever URL the model passes leaves the machine verbatim with the user's
 * identity. Hence the deliberate narrowness — http/https only (no file:, data: or
 * other schemes that would read the host through a URL), no credential injection, and
 * headers only as the program spells them out.
 *
 * Three limits keep one call from wrecking a turn: a 30s deadline (the program cap is
 * 3 minutes and covers the whole round), a 1MB body cap with an explicit `truncated`
 * flag (a giant page must not silently become the model's context), and the turn's
 * interrupt signal — an interrupt terminates the worker but would otherwise leave the
 * request in flight.
 *
 * Failures throw, catchably, with the URL and status in the message: a program that
 * fetched nothing must not mistake an empty body for a real answer.
 */

/** Body cap: bigger responses come back cut, with truncated: true. */
const MAX_BYTES = 1_000_000;
/** One request must never stall a turn; the 3-minute program cap is not a deadline. */
const DEADLINE_MS = 30_000;

export interface FetchOptions {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

export interface FetchResult {
  status: number;
  ok: boolean;
  /** The FINAL url — redirects are followed, so this may differ from the request. */
  url: string;
  contentType: string;
  body: string;
  truncated: boolean;
}

/** GET/POST/… a URL from the host and return the response as text. Throws on transport failure. */
export async function fetchUrl(
  url: string,
  opts: FetchOptions = {},
  signal?: AbortSignal,
): Promise<FetchResult> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error(`fetch: not a valid URL: ${url}`);
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error(`fetch: only http/https URLs are allowed (got ${parsed.protocol}${url})`);
  }

  // Two abort sources — the deadline and the turn's interrupt — folded into the one
  // signal fetch takes. The timer is cleared on every exit path so a finished call
  // cannot keep the process alive.
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(new Error("deadline")), DEADLINE_MS);
  const onInterrupt = () => ctl.abort(new Error("interrupted"));
  signal?.addEventListener("abort", onInterrupt, { once: true });

  try {
    const res = await fetch(parsed, {
      method: opts.method ?? "GET",
      headers: opts.headers,
      body: opts.body,
      signal: ctl.signal,
      redirect: "follow",
    });
    // Read at most MAX_BYTES, then drop the rest: `res.text()` on a multi-GB
    // download would buffer all of it host-side before we could cap anything.
    const buf = new Uint8Array(MAX_BYTES);
    let n = 0;
    let truncated = false;
    const reader = res.body?.getReader();
    while (reader) {
      const { done, value } = await reader.read();
      if (done) break;
      const room = MAX_BYTES - n;
      if (value.length >= room) {
        buf.set(value.subarray(0, room), n);
        n = MAX_BYTES;
        truncated = true;
        await reader.cancel();
        break;
      }
      buf.set(value, n);
      n += value.length;
    }
    return {
      status: res.status,
      ok: res.ok,
      url: res.url || parsed.href,
      contentType: res.headers.get("content-type") ?? "",
      body: new TextDecoder().decode(buf.subarray(0, n)),
      truncated,
    };
  } catch (err) {
    // AbortError carries no useful text — say WHICH abort it was, since "the turn was
    // interrupted" and "the server took >30s" call for different next moves.
    const reason = ctl.signal.aborted
      ? (ctl.signal.reason as Error)?.message === "interrupted"
        ? "interrupted"
        : `timed out after ${DEADLINE_MS / 1000}s`
      : (err as Error).message;
    throw new Error(`fetch ${parsed.href} failed: ${reason}`);
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", onInterrupt);
  }
}
