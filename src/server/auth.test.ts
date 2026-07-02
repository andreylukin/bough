import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";

function ctx(password?: string): AppCtx {
  const bus = new Bus();
  return { db: new Db(":memory:"), bus, password };
}

const req = (method: string, path: string, init?: RequestInit) =>
  new Request("http://x" + path, { method, ...init });

Deno.test("no password → requests pass through untouched", async () => {
  const c = ctx();
  const h = createHandler(c);
  assertEquals((await h(req("GET", "/sessions"))).status, 200);
  c.db.close();
});

Deno.test("password set → API without cookie is 401; browser GET gets the login page", async () => {
  const c = ctx("hunter2");
  const h = createHandler(c);

  const api = await h(req("GET", "/sessions"));
  assertEquals(api.status, 401);
  assertEquals((await api.json()).error, "unauthorized");

  const page = await h(req("GET", "/", { headers: { accept: "text/html" } }));
  assertEquals(page.status, 401);
  assertStringIncludes(await page.text(), "/auth/login");
  c.db.close();
});

Deno.test("wrong password → 401, no cookie; right password → cookie unlocks the API", async () => {
  const c = ctx("hunter2");
  const h = createHandler(c);
  const login = (password: string) =>
    h(req("POST", "/auth/login", {
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password }),
    }));

  const bad = await login("nope");
  assertEquals(bad.status, 401);
  assertEquals(bad.headers.get("set-cookie"), null);
  await bad.body?.cancel();

  const good = await login("hunter2");
  assertEquals(good.status, 200);
  const cookie = good.headers.get("set-cookie")!;
  assertStringIncludes(cookie, "bough_session=");
  assertStringIncludes(cookie, "HttpOnly");
  await good.body?.cancel();

  const authed = await h(req("GET", "/sessions", {
    headers: { cookie: cookie.split(";")[0] },
  }));
  assertEquals(authed.status, 200);

  // A made-up token is not a session.
  const forged = await h(req("GET", "/sessions", {
    headers: { cookie: "bough_session=forged" },
  }));
  assertEquals(forged.status, 401);
  c.db.close();
});

Deno.test("form login → 303 to / with the session cookie", async () => {
  const c = ctx("hunter2");
  const h = createHandler(c);
  const form = new URLSearchParams({ password: "hunter2" });
  const res = await h(req("POST", "/auth/login", {
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: form.toString(),
  }));
  assertEquals(res.status, 303);
  assertEquals(res.headers.get("location"), "/");
  assert(res.headers.get("set-cookie")?.includes("bough_session="));
  c.db.close();
});
