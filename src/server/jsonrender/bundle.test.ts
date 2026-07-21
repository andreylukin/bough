import { assert, assertEquals } from "jsr:@std/assert@1";
import { viewerBundle, viewerPage } from "./bundle.ts";

Deno.test("viewerPage inlines the spec safely and carries the viewer contract", () => {
  const html = viewerPage('{"root":"p","elements":{}}', 'A <"title">');
  assert(html.includes('id="__ui_spec__"'));
  assert(html.includes('src="/artifact-viewer.js"'));
  assert(html.includes("AI-generated"));
  assert(html.includes("</body>"), "widget injection point present");
  assert(html.includes("<title>A &lt;&quot;title&quot;&gt;</title>"), "title escaped");
  // spec content can never close its script tag
  const sneaky = viewerPage('{"x":"</script><script>alert(1)"}', "t");
  assert(!sneaky.includes("</script><script>alert(1)"));
});

Deno.test("viewerBundle builds a browser bundle once and caches it by source hash", async () => {
  const base = await Deno.makeTempDir({ prefix: "viewer-bundle-" });
  const first = await viewerBundle(base);
  assert(first.js.includes("__ui_spec__"), "bundle contains the viewer entry");
  assert(first.js.length > 100_000, "react-dom is bundled in");
  const again = await viewerBundle(base);
  assertEquals(again.etag, first.etag);
  await Deno.remove(base, { recursive: true });
});
