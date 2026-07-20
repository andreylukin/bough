import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import {
  addComment,
  commentWidget,
  deleteComment,
  formatForAgent,
  loadComments,
  markSent,
} from "./comments.ts";
import { artifactsRoot, listArtifacts, publishArtifact, serveArtifact } from "./artifacts.ts";

function tmp(): string {
  return Deno.makeTempDirSync({ prefix: "comments-" });
}

const anchor = { label: "Files touched", selector: "body > h2", xf: 0.5, yf: 0.3 };

Deno.test("addComment persists; loadComments reads back; delete removes", () => {
  const base = tmp();
  const c = addComment("s1", { artifact: "index.html", text: "this list is stale", anchor }, base);
  assert(c.id);
  assertEquals(c.sent, false);
  assertEquals(loadComments("s1", base).length, 1);
  assertEquals(loadComments("s1", base)[0].text, "this list is stale");
  assertEquals(deleteComment("s1", c.id, base), true);
  assertEquals(loadComments("s1", base).length, 0);
  assertEquals(deleteComment("s1", "nope", base), false);
  Deno.removeSync(base, { recursive: true });
});

Deno.test("the comments sidecar is not walked as an artifact", async () => {
  const base = tmp();
  await publishArtifact("s2", "index.html", "<h1>hi</h1>", base);
  addComment("s2", { artifact: "index.html", text: "note", anchor }, base);
  const arts = await listArtifacts("s2", base);
  assertEquals(arts.map((a) => a.name), ["index.html"]); // no ".comments.json"
  Deno.removeSync(base, { recursive: true });
});

Deno.test("markSent flips the sent flag; formatForAgent groups by artifact", () => {
  const base = tmp();
  const a = addComment("s3", { artifact: "index.html", text: "fix this", anchor }, base);
  addComment("s3", { artifact: "chart.html", text: "wrong axis", anchor }, base);
  markSent("s3", [a.id], base);
  const all = loadComments("s3", base);
  assertEquals(all.find((c) => c.id === a.id)!.sent, true);

  const note = formatForAgent(all);
  assert(note.startsWith("[artifact comments]"));
  assert(note.includes('On the artifact "index.html"'));
  assert(note.includes('On the artifact "chart.html"'));
  assert(note.includes('(near "Files touched") fix this'));
  Deno.removeSync(base, { recursive: true });
});

Deno.test("serveArtifact injects the comment layer into HTML only", async () => {
  const base = tmp();
  await publishArtifact("s4", "index.html", "<html><body><h1>hi</h1></body></html>", base);
  await publishArtifact("s4", "app.js", "console.log(1)", base);
  const html = await (await serveArtifact("s4", "index.html", base)).text();
  assert(html.includes("bgh-cmt-toggle")); // widget injected
  assert(html.includes("<h1>hi</h1>")); // original content preserved
  assert(html.indexOf("bgh-cmt") < html.lastIndexOf("</body>") + "</body>".length);
  const js = await (await serveArtifact("s4", "app.js", base)).text();
  assertEquals(js, "console.log(1)"); // non-HTML untouched
  Deno.removeSync(base, { recursive: true });
});

Deno.test("the widget is self-contained: no external network references", () => {
  const w = commentWidget();
  assert(!/src=["']https?:/i.test(w));
  assert(!/href=["']https?:/i.test(w));
  assert(!/cdn\.|googleapis|unpkg|jsdelivr/i.test(w));
});

Deno.test("a non-token session id can't write outside the store", () => {
  const base = tmp();
  // Read is safe-empty (no path is touched); write rejects the bad id outright.
  assertEquals(loadComments("../evil", base), []);
  let threw = false;
  try {
    addComment("../evil", { artifact: "x", text: "y", anchor }, base);
  } catch {
    threw = true;
  }
  assert(threw);
  Deno.removeSync(base, { recursive: true });
});

// The sidecar sits beside the session dir, never inside it (traversal-safe).
Deno.test("sidecar lives beside the session dir", () => {
  const base = tmp();
  addComment("s5", { artifact: "index.html", text: "x", anchor }, base);
  assert(Deno.statSync(join(artifactsRoot(base), "s5.comments.json")).isFile);
  Deno.removeSync(base, { recursive: true });
});
