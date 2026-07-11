import { assertEquals } from "jsr:@std/assert@1";
import { isSsoExpired, makeHandler, SsoExpired, toContainerCreds } from "./cred-broker.ts";

Deno.test("toContainerCreds maps SessionToken -> Token, defaults Expiration", () => {
  const c = toContainerCreds(JSON.stringify({
    Version: 1,
    AccessKeyId: "AKIA",
    SecretAccessKey: "shh",
    SessionToken: "sess",
    Expiration: "2030-01-01T00:00:00Z",
  }));
  assertEquals(c.AccessKeyId, "AKIA");
  assertEquals(c.Token, "sess"); // renamed for the container-credentials endpoint
  assertEquals(c.Expiration, "2030-01-01T00:00:00Z");

  // No Expiration → a future default is filled in (parseable).
  const d = toContainerCreds(JSON.stringify({ AccessKeyId: "A", SecretAccessKey: "s", SessionToken: "t" }));
  assertEquals(Number.isFinite(Date.parse(d.Expiration)), true);
});

Deno.test("isSsoExpired recognizes the re-login stderrs", () => {
  assertEquals(isSsoExpired("Error loading SSO Token: expired"), true);
  assertEquals(isSsoExpired("The SSO session associated has expired"), true);
  assertEquals(isSsoExpired("Unable to locate credentials"), false);
});

Deno.test("makeHandler: 401 without bearer, 404 unknown path, 503 on SSO expiry, 200 on success", async () => {
  const good: Record<string, () => Promise<any>> = {
    "/aws": () => Promise.resolve({ AccessKeyId: "A", SecretAccessKey: "s", Token: "t", Expiration: "x" }),
    "/aws-admin": () => Promise.reject(new SsoExpired("default")),
  };
  const h = makeHandler("tok", good, { "/aws": "bough-ro", "/aws-admin": "default" });
  const url = "http://127.0.0.1/aws";

  assertEquals((await h(new Request(url))).status, 401); // no bearer
  assertEquals((await h(new Request(url, { headers: { authorization: "Bearer nope" } }))).status, 401);
  assertEquals((await h(new Request("http://127.0.0.1/nope", { headers: { authorization: "Bearer tok" } }))).status, 404);

  const ok = await h(new Request(url, { headers: { authorization: "Bearer tok" } }));
  assertEquals(ok.status, 200);
  assertEquals((await ok.json()).Token, "t");

  const expired = await h(new Request("http://127.0.0.1/aws-admin", { headers: { authorization: "Bearer tok" } }));
  assertEquals(expired.status, 503);
  assertEquals((await expired.text()).includes("aws sso login --profile default"), true);
});
