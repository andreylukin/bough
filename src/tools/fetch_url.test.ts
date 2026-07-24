import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { fetchUrl } from "./fetch_url.ts";

/** A loopback server for the duration of one test; returns its origin + a stop fn. */
function serve(handler: (req: Request) => Response | Promise<Response>) {
  const server = Deno.serve({ port: 0, onListen: () => {} }, handler);
  return {
    origin: `http://127.0.0.1:${(server.addr as Deno.NetAddr).port}`,
    stop: () => server.shutdown(),
  };
}

Deno.test("a GET returns the structured response", async () => {
  const s = serve(() => new Response("hello", { headers: { "content-type": "text/plain" } }));
  try {
    const res = await fetchUrl(`${s.origin}/x`);
    assertEquals(res.status, 200);
    assertEquals(res.ok, true);
    assertEquals(res.body, "hello");
    assertEquals(res.truncated, false);
    assertStringIncludes(res.contentType, "text/plain");
  } finally {
    await s.stop();
  }
});

Deno.test("method, headers and body reach the server; a non-2xx status is data", async () => {
  let seen = "";
  let auth = "";
  const s = serve(async (req) => {
    seen = `${req.method} ${await req.text()}`;
    auth = req.headers.get("x-token") ?? "";
    return new Response("nope", { status: 418 });
  });
  try {
    const res = await fetchUrl(`${s.origin}/`, {
      method: "POST",
      headers: { "x-token": "t" },
      body: "payload",
    });
    assertEquals(seen, "POST payload");
    assertEquals(auth, "t");
    assertEquals(res.status, 418);
    assertEquals(res.ok, false);
    assertEquals(res.body, "nope");
  } finally {
    await s.stop();
  }
});

Deno.test("an oversized body comes back cut and flagged", async () => {
  const s = serve(() => new Response("x".repeat(1_200_000)));
  try {
    const res = await fetchUrl(`${s.origin}/`);
    assertEquals(res.truncated, true);
    assertEquals(res.body.length, 1_000_000);
  } finally {
    await s.stop();
  }
});

Deno.test("non-http schemes are refused", async () => {
  await assertRejects(
    () => fetchUrl("file:///etc/passwd"),
    Error,
    "only http/https",
  );
  await assertRejects(() => fetchUrl("not a url"), Error, "not a valid URL");
});

Deno.test("an interrupted turn aborts the request", async () => {
  const ctl = new AbortController();
  const s = serve(() => new Promise<Response>(() => {})); // never answers
  try {
    const p = assertRejects(
      () => fetchUrl(`${s.origin}/`, {}, ctl.signal),
      Error,
      "interrupted",
    );
    ctl.abort();
    await p;
  } finally {
    await s.stop();
  }
});
