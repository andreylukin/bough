/**
 * Host `fetch()` (T6.5).
 *
 * THE THREE THINGS THIS FILE EXISTS TO PIN, all from the spec's one-line contract
 * ("non-2xx is data, 1MB cap with `truncated`, 30s deadline"):
 *
 *   1. **A non-2xx is DATA.** A 404 and a 500 resolve, carrying status and body.
 *      Only a bad URL, a transport failure, the deadline and the interrupt reject.
 *   2. **The truncation flag is set when the cap bites** — and the body really is the
 *      capped prefix, not a silently-cut document the program parses as complete.
 *   3. **The deadline fires and says which abort it was**, distinctly from an
 *      interrupt, because those two call for different next moves.
 *
 * Hermetic with no socket: `fetchImpl` is injected, so every case is a fabricated
 * `Response` or a promise that never settles. Nothing here touches the network, and
 * the deadline runs in milliseconds rather than thirty seconds.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. (Same constraint `hostfn/shell.test.ts` and `bus.test.ts` document.)
 */

import { test } from "bun:test";
import assert from "node:assert";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { NetError } from "../errors.ts";
import type { TurnCtx } from "../types.ts";
import {
  createFetchHostFn,
  type FetchImpl,
  type FetchResult,
  fetchUrl,
  MAX_BYTES,
} from "./fetch.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** A fetch that answers every request with the same response. */
function respondWith(res: () => Response): FetchImpl {
  return () => Promise.resolve(res());
}

/** A fetch that records what it was called with, then answers 200. */
function recorder(calls: { url: string; init?: RequestInit }[]): FetchImpl {
  return (input: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return Promise.resolve(new Response("ok", { status: 200 }));
  };
}

/** A body of `size` bytes, delivered in `chunk`-sized pieces. */
function streamOf(size: number, chunk: number, fill = 97): Response {
  let sent = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (sent >= size) return controller.close();
      const n = Math.min(chunk, size - sent);
      sent += n;
      controller.enqueue(new Uint8Array(n).fill(fill));
    },
  });
  return new Response(stream, { status: 200, headers: { "content-type": "text/plain" } });
}

function turnCtx(signal: AbortSignal): TurnCtx {
  return {
    db: openDb(":memory:"),
    bus: new Bus(),
    sessionId: "session-1",
    turnId: "turn-1",
    messageId: "message-1",
    workspace: "/work",
    model: "test-model",
    signal,
    depth: 0,
  };
}

async function rejectsWith(fn: () => unknown, fragment: string): Promise<NetError> {
  try {
    await fn();
  } catch (err) {
    assert.ok(err instanceof NetError, `expected NetError, got ${err}`);
    assert.ok(
      err.message.includes(fragment),
      `expected message to mention ${JSON.stringify(fragment)}, got: ${err.message}`,
    );
    return err;
  }
  assert.fail("expected a NetError");
}

// ---------------------------------------------------------------------------
// A response is data
// ---------------------------------------------------------------------------

test("a 200 comes back with status, body, contentType and the final url", async () => {
  const result = await fetchUrl("https://example.test/a", {}, undefined, {
    fetchImpl: respondWith(() =>
      new Response("hello", {
        status: 200,
        headers: { "content-type": "text/plain; charset=utf-8" },
      })
    ),
  });
  assert.equal(result.status, 200);
  assert.equal(result.ok, true);
  assert.equal(result.body, "hello");
  assert.equal(result.contentType, "text/plain; charset=utf-8");
  assert.equal(result.truncated, false);
  assert.equal(result.url, "https://example.test/a");
});

test("a 404 is DATA, not an exception", async () => {
  const result = await fetchUrl("https://example.test/missing", {}, undefined, {
    fetchImpl: respondWith(() => new Response("no such page", { status: 404 })),
  });
  assert.equal(result.ok, false);
  assert.equal(result.status, 404);
  assert.equal(result.body, "no such page", "the error body is what tells the model why");
});

test("a 500 with a body is DATA too", async () => {
  const result = await fetchUrl("https://example.test/boom", {}, undefined, {
    fetchImpl: respondWith(() => new Response('{"error":"upstream"}', { status: 500 })),
  });
  assert.equal(result.ok, false);
  assert.equal(result.status, 500);
  assert.deepEqual(JSON.parse(result.body), { error: "upstream" });
});

test("every non-2xx status resolves rather than throwing", async () => {
  for (const status of [201, 204, 301, 400, 401, 403, 429, 503]) {
    const result = await fetchUrl("https://example.test/s", {}, undefined, {
      fetchImpl: respondWith(() =>
        new Response(status === 204 ? null : String(status), { status })
      ),
    });
    assert.equal(result.status, status);
    assert.equal(result.ok, status >= 200 && status < 300);
  }
});

test("the FINAL url is reported, so a redirect is visible", async () => {
  // What `redirect: "follow"` produces: a response whose `url` is where it landed.
  // `Response.url` is a read-only getter, so a fabricated one is defined onto it.
  const landed = new Response("body", { status: 200 });
  Object.defineProperty(landed, "url", { value: "https://example.test/long" });
  const result = await fetchUrl("https://example.test/short", {}, undefined, {
    fetchImpl: respondWith(() => landed),
  });
  assert.equal(result.url, "https://example.test/long");
});

// ---------------------------------------------------------------------------
// The truncation flag
// ---------------------------------------------------------------------------

test("a body over the cap comes back cut, with truncated: true", async () => {
  const cap = 1_000;
  const result = await fetchUrl("https://example.test/big", {}, undefined, {
    fetchImpl: respondWith(() => streamOf(10_000, 256)),
    maxBytes: cap,
  });
  assert.equal(result.truncated, true, "a silently cut body is the failure this flag prevents");
  assert.equal(result.body.length, cap);
  assert.equal(result.body, "a".repeat(cap));
});

test("a body exactly at the cap is not flagged truncated", async () => {
  const cap = 512;
  const result = await fetchUrl("https://example.test/exact", {}, undefined, {
    fetchImpl: respondWith(() => streamOf(cap, cap)),
    maxBytes: cap,
  });
  assert.equal(result.body.length, cap);
  assert.equal(result.truncated, false);
});

test("a body under the cap is whole and unflagged", async () => {
  const result = await fetchUrl("https://example.test/small", {}, undefined, {
    fetchImpl: respondWith(() => streamOf(100, 7)),
    maxBytes: 1_000,
  });
  assert.equal(result.body.length, 100);
  assert.equal(result.truncated, false);
});

test("an empty body is empty, not truncated", async () => {
  const result = await fetchUrl("https://example.test/none", {}, undefined, {
    fetchImpl: respondWith(() => new Response(null, { status: 204 })),
  });
  assert.equal(result.body, "");
  assert.equal(result.truncated, false);
});

test("the production cap is 1MB", () => {
  assert.equal(MAX_BYTES, 1_000_000);
});

// ---------------------------------------------------------------------------
// The deadline and the interrupt
// ---------------------------------------------------------------------------

/** A fetch that never answers, but honors the signal it is given. */
const neverAnswers: FetchImpl = (_input, init) =>
  new Promise((_resolve, reject) => {
    const signal = init?.signal;
    if (!signal) return;
    if (signal.aborted) return reject(new DOMException("aborted", "AbortError"));
    signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), {
      once: true,
    });
  });

test("the deadline aborts the request and says so", async () => {
  const err = await rejectsWith(
    () =>
      fetchUrl("https://example.test/slow", {}, undefined, {
        fetchImpl: neverAnswers,
        deadlineMs: 10,
      }),
    "no response within",
  );
  assert.match(err.message, /https:\/\/example\.test\/slow/, "the url must be in the message");
});

test("an interrupt is distinguishable from the deadline", async () => {
  const controller = new AbortController();
  const pending = fetchUrl("https://example.test/slow", {}, controller.signal, {
    fetchImpl: neverAnswers,
    deadlineMs: 60_000,
  });
  controller.abort();
  const err = await rejectsWith(() => pending, "the turn was interrupted");
  // The two reasons must not read alike: one says retry, the other says stop.
  assert.ok(!err.message.includes("no response within"));
});

test("an already-interrupted turn does not start the request", async () => {
  const controller = new AbortController();
  controller.abort();
  await rejectsWith(
    () =>
      fetchUrl("https://example.test/x", {}, controller.signal, {
        fetchImpl: neverAnswers,
        deadlineMs: 60_000,
      }),
    "the turn was interrupted",
  );
});

test("a transport failure reports the underlying reason", async () => {
  await rejectsWith(
    () =>
      fetchUrl("https://nope.invalid/x", {}, undefined, {
        fetchImpl: () => Promise.reject(new TypeError("error sending request: dns error")),
      }),
    "dns error",
  );
});

// ---------------------------------------------------------------------------
// URLs and options
// ---------------------------------------------------------------------------

test("only http and https are allowed", async () => {
  for (const url of ["file:///etc/passwd", "data:text/plain,hi", "ftp://host/x"]) {
    await rejectsWith(
      () => fetchUrl(url, {}, undefined, { fetchImpl: respondWith(() => new Response("x")) }),
      "only http and https",
    );
  }
});

test("a URL that does not parse is refused before any request", async () => {
  let called = false;
  await rejectsWith(
    () =>
      fetchUrl("not a url", {}, undefined, {
        fetchImpl: () => {
          called = true;
          return Promise.resolve(new Response("x"));
        },
      }),
    "is not a valid URL",
  );
  assert.equal(called, false);
});

test("method, headers and body reach the request", async () => {
  const calls: { url: string; init?: RequestInit }[] = [];
  await fetchUrl(
    "https://example.test/post",
    { method: "POST", headers: { "x-token": "abc" }, body: '{"a":1}' },
    undefined,
    { fetchImpl: recorder(calls) },
  );
  assert.equal(calls.length, 1);
  assert.equal(calls[0].init?.method, "POST");
  assert.deepEqual(calls[0].init?.headers, { "x-token": "abc" });
  assert.equal(calls[0].init?.body, '{"a":1}');
  assert.equal(calls[0].init?.redirect, "follow");
});

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

test("the fetch host fn returns the result as JSON", async () => {
  const ctx = turnCtx(new AbortController().signal);
  const { fetch } = createFetchHostFn(ctx, {
    fetchImpl: respondWith(() =>
      new Response("body", { status: 418, headers: { "content-type": "text/plain" } })
    ),
  });
  const result = JSON.parse(await fetch!("https://example.test/t", "{}")) as FetchResult;
  assert.deepEqual(result, {
    status: 418,
    ok: false,
    url: "https://example.test/t",
    contentType: "text/plain",
    body: "body",
    truncated: false,
  });
  ctx.db.close();
});

test("the fetch host fn carries the turn's interrupt", async () => {
  const controller = new AbortController();
  const ctx = turnCtx(controller.signal);
  const { fetch } = createFetchHostFn(ctx, { fetchImpl: neverAnswers, deadlineMs: 60_000 });
  const pending = fetch!("https://example.test/slow", "{}");
  controller.abort();
  await rejectsWith(() => pending, "the turn was interrupted");
  ctx.db.close();
});

test("the fetch host fn rejects unknown options rather than ignoring them", async () => {
  const ctx = turnCtx(new AbortController().signal);
  const { fetch } = createFetchHostFn(ctx, {
    fetchImpl: respondWith(() => new Response("x")),
  });
  await rejectsWith(
    () => fetch!("https://example.test/x", JSON.stringify({ timeout: 5 })),
    "invalid options",
  );
  ctx.db.close();
});

test("the fetch host fn rejects a non-JSON options argument", async () => {
  const ctx = turnCtx(new AbortController().signal);
  const { fetch } = createFetchHostFn(ctx, {
    fetchImpl: respondWith(() => new Response("x")),
  });
  await rejectsWith(() => fetch!("https://example.test/x", "{oops"), "not valid JSON");
  ctx.db.close();
});
