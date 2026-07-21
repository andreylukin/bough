/**
 * Viewer delivery for `*.ui.json` artifacts: the wrapper page that inlines a
 * validated spec, and the browser bundle that renders it.
 *
 * The bundle is built lazily with `deno bundle --platform browser` from this repo's
 * sources (viewer.tsx → registry.tsx → catalog.ts) the first time it is requested,
 * then cached under ~/.bough/cache keyed by a hash of those sources — so a source
 * checkout serves fresh code after `bough restart` with no separate build step, and
 * subsequent serves are a file read. The server itself is the deno binary
 * (Deno.execPath()), so no external toolchain is assumed.
 */
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { VIEWER_CSS } from "./styles.ts";

/** URL the wrapper page loads the bundle from (routed in app.ts). */
export const VIEWER_JS_PATH = "/artifact-viewer.js";

const SOURCES = ["viewer.tsx", "registry.tsx", "catalog.ts", "styles.ts", "../../../deno.json"];

async function sourceHash(): Promise<string> {
  const dir = dirname(fileURLToPath(import.meta.url));
  let text = "";
  for (const s of SOURCES) text += await Deno.readTextFile(join(dir, s));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].slice(0, 12).map((b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
}

let memo: Promise<{ js: string; etag: string }> | null = null;

async function build(base?: string): Promise<{ js: string; etag: string }> {
  const dir = dirname(fileURLToPath(import.meta.url));
  const hash = await sourceHash();
  const home = base ?? join(Deno.env.get("HOME") ?? ".", ".bough");
  const cached = join(home, "cache", `artifact-viewer-${hash}.js`);
  try {
    return { js: await Deno.readTextFile(cached), etag: hash };
  } catch {
    // not cached yet — build below
  }
  const out = await new Deno.Command(Deno.execPath(), {
    args: [
      "bundle",
      "--quiet",
      "--platform",
      "browser",
      "--minify",
      "--config",
      join(dir, "../../../deno.json"),
      "-o",
      cached + ".tmp",
      join(dir, "viewer.tsx"),
    ],
    stdout: "piped",
    stderr: "piped",
    cwd: dirname(join(dir, "../../../deno.json")),
  }).output();
  if (!out.success) {
    throw new Error(`viewer bundle failed: ${new TextDecoder().decode(out.stderr)}`);
  }
  await Deno.mkdir(dirname(cached), { recursive: true });
  await Deno.rename(cached + ".tmp", cached);
  return { js: await Deno.readTextFile(cached), etag: hash };
}

/** The viewer bundle (built/cached on first call). `base` overrides ~/.bough for tests. */
export function viewerBundle(base?: string): Promise<{ js: string; etag: string }> {
  memo ??= build(base).catch((e) => {
    memo = null; // a failed build retries on the next request
    throw e;
  });
  return memo;
}

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/**
 * The HTML wrapper served for a `*.ui.json` artifact: inline styles + inline spec
 * JSON + the viewer bundle. Served as text/html, so artifacts.ts injects the
 * comment layer into it like any other HTML artifact.
 */
export function viewerPage(specJson: string, title: string): string {
  // `<` is escaped so spec content can never close the script tag.
  const safeSpec = specJson.replace(/</g, "\\u003c");
  return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)}</title>
<style>${VIEWER_CSS}</style>
</head>
<body>
<div id="root"></div>
<script id="__ui_spec__" type="application/json">${safeSpec}</script>
<script type="module" src="${VIEWER_JS_PATH}"></script>
<footer class="b-foot"><span>AI-generated — verify anything important</span><a href="?raw=1">spec</a></footer>
</body>
</html>`;
}
