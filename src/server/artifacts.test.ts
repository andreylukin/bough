import { assert, assertEquals, assertRejects } from "jsr:@std/assert@1";
import { join } from "node:path";
import { artifactsRoot, listArtifacts, publishArtifact, serveArtifact } from "./artifacts.ts";

function tmp(): string {
  return Deno.makeTempDirSync({ prefix: "artifacts-" });
}

Deno.test("publishArtifact writes under the session dir and returns url/href", async () => {
  const base = tmp();
  const art = await publishArtifact("sessAbc", "index.html", "<h1>hi</h1>", base);
  assertEquals(art.name, "index.html");
  assertEquals(art.url, "/artifacts/sessAbc/index.html");
  assert(art.href.endsWith("/artifacts/sessAbc/index.html"));
  assert(art.href.startsWith("http://127.0.0.1:"));
  assertEquals(art.bytes, "<h1>hi</h1>".length);
  const onDisk = await Deno.readTextFile(join(artifactsRoot(base), "sessAbc", "index.html"));
  assertEquals(onDisk, "<h1>hi</h1>");
  await Deno.remove(base, { recursive: true });
});

Deno.test("publishArtifact supports nested paths and overwrites in place", async () => {
  const base = tmp();
  await publishArtifact("s1", "assets/app.js", "v1", base);
  const two = await publishArtifact("s1", "assets/app.js", "v2-longer", base);
  assertEquals(two.name, "assets/app.js");
  assertEquals(two.url, "/artifacts/s1/assets/app.js");
  const onDisk = await Deno.readTextFile(join(artifactsRoot(base), "s1", "assets", "app.js"));
  assertEquals(onDisk, "v2-longer");
  await Deno.remove(base, { recursive: true });
});

Deno.test("listArtifacts returns every file newest-first; absent → []", async () => {
  const base = tmp();
  assertEquals(await listArtifacts("nope", base), []);
  await publishArtifact("s2", "a.html", "a", base);
  await new Promise((r) => setTimeout(r, 5));
  await publishArtifact("s2", "sub/b.css", "b", base);
  const list = await listArtifacts("s2", base);
  assertEquals(list.map((a) => a.name).sort(), ["a.html", "sub/b.css"]);
  // newest first: sub/b.css published last
  assertEquals(list[0].name, "sub/b.css");
  await Deno.remove(base, { recursive: true });
});

Deno.test("serveArtifact returns the file with the right content-type", async () => {
  const base = tmp();
  await publishArtifact("s3", "page.html", "<!doctype html><title>x</title>", base);
  const res = await serveArtifact("s3", "page.html", base);
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("content-type"), "text/html; charset=utf-8");
  assert((await res.text()).includes("<title>x</title>"));
  await Deno.remove(base, { recursive: true });
});

Deno.test("serveArtifact 404s a missing file, not HTML", async () => {
  const base = tmp();
  const res = await serveArtifact("s4", "ghost.html", base);
  assertEquals(res.status, 404);
  assertEquals(res.headers.get("content-type"), "application/json");
  await Deno.remove(base, { recursive: true });
});

Deno.test("traversal in the name or a bad session id is blocked", async () => {
  const base = tmp();
  const res = await serveArtifact("s5", "../../../../etc/passwd", base);
  assertEquals(res.status, 403);
  await assertRejects(() => publishArtifact("s6", "../escape.html", "x", base));
  await assertRejects(() => publishArtifact("../evil", "a.html", "x", base));
  await Deno.remove(base, { recursive: true });
});
