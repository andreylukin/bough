import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { setupGithub } from "./github.ts";

Deno.test("setupGithub: undefined when gh is absent or unauthenticated", async () => {
  assertEquals(await setupGithub(() => Promise.resolve(undefined)), undefined);
  assertEquals(await setupGithub(() => Promise.resolve("")), undefined);
});

Deno.test("setupGithub: Basic on github.com, token on api.github.com", async () => {
  const setup = (await setupGithub(() => Promise.resolve("gho_abc")))!;
  const byHost = new Map(setup.credentials.map((c) => [c.host, c]));
  assertEquals([...byHost.keys()].sort(), ["api.github.com", "github.com"]);

  const git = byHost.get("github.com")!;
  assertEquals(git.header, "authorization");
  const gitValue = await (git.value as () => Promise<string>)();
  assertEquals(gitValue, `Basic ${btoa("x-access-token:gho_abc")}`);

  const api = await (byHost.get("api.github.com")!.value as () => Promise<string>)();
  assertEquals(api, "token gho_abc");
});

Deno.test("setupGithub: token re-read after TTL, cached within it", async () => {
  let calls = 0;
  const setup = (await setupGithub(() => Promise.resolve(`t${++calls}`)))!;
  const mint = setup.credentials[1].value as () => Promise<string>;
  assertEquals(await mint(), "token t2"); // probe consumed t1
  assertEquals(await mint(), "token t2"); // cached — no new subprocess
  assertEquals(calls, 2);
});

Deno.test("setupGithub: mint failure names the fix", async () => {
  let first = true;
  const setup = (await setupGithub(() => {
    const t = first ? "gho_abc" : undefined;
    first = false;
    return Promise.resolve(t);
  }))!;
  const mint = setup.credentials[0].value as () => Promise<string>;
  const err = await mint().then(() => "", (e: Error) => e.message);
  assertStringIncludes(err, "gh auth login");
});
