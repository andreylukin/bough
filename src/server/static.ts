/**
 * Static serving of the built web UI (web/dist) from the same :4321 origin as the API,
 * so one process serves everything and the SPA's absolute asset paths (/assets/…,
 * /favicon.svg — see web/dist/index.html) resolve. This is the production path; in dev
 * the Vite server proxies to :4321 instead.
 *
 * SPA fallback: a GET that doesn't resolve to a file returns index.html, so client-side
 * routes (deep links like /sessions/:id/turn/:n) load the app rather than 404. The API
 * router runs first in app.ts, so this only ever sees non-API GETs.
 *
 * Dependency-light on purpose (matches policy.ts): a small MIME table + Deno.readFile,
 * no @std/http. Path traversal is blocked by resolving under the root and checking the
 * prefix. Hashed assets get a long immutable cache; index.html is always revalidated.
 */
import { normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".ico": "image/x-icon",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
  ".map": "application/json; charset=utf-8",
  ".webmanifest": "application/manifest+json",
  ".txt": "text/plain; charset=utf-8",
};

function contentType(path: string): string {
  const dot = path.lastIndexOf(".");
  return (dot >= 0 && MIME[path.slice(dot).toLowerCase()]) || "application/octet-stream";
}

/** The build output dir, resolved relative to this module: web/dist. */
export function defaultWebDir(): string {
  return fileURLToPath(new URL("../../web/dist", import.meta.url));
}

async function fileResponse(path: string, immutable: boolean): Promise<Response> {
  const body = await Deno.readFile(path);
  return new Response(body, {
    headers: {
      "content-type": contentType(path),
      "cache-control": immutable ? "public, max-age=31536000, immutable" : "no-cache",
    },
  });
}

// Shown when web/dist doesn't exist yet (the web-ui build hasn't run). Keeps
// `deno task desktop` / a browser at :4321 showing a real page instead of a raw error.
const PLACEHOLDER = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>bough</title><style>body{font:15px/1.6 system-ui,sans-serif;background:#15171a;
color:#d8dde3;display:grid;place-items:center;height:100vh;margin:0}code{color:#7ec699}
</style></head><body><div><h1>bough</h1><p>The web UI isn't built yet.</p>
<p>Run <code>cd web &amp;&amp; npm run build</code>, then reload.</p></div></body></html>`;

let warnedMissing = false;
function warnMissingBuildOnce(root: string): void {
  if (warnedMissing) return;
  warnedMissing = true;
  console.warn(`[static] no web build at ${root} — serving placeholder. Run: cd web && npm run build`);
}

/**
 * Serve a non-API GET from the web build:
 *   - an existing file → itself;
 *   - a path that looks like a missing asset (has an extension) → 404 (don't hand back
 *     HTML for a `.js`/`.css`, which would just confuse the browser);
 *   - any other path (a client route) → index.html (SPA fallback), or a placeholder page
 *     with a build hint if dist isn't there yet.
 * `dir` is the build root.
 */
export async function serveWeb(req: Request, dir: string): Promise<Response> {
  const root = resolve(dir);
  const pathname = decodeURIComponent(new URL(req.url).pathname);
  const target = normalize(resolve(root, "." + pathname));

  // Traversal guard: the resolved path must stay under the root.
  if (target !== root && !target.startsWith(root + "/")) {
    return new Response("forbidden", { status: 403 });
  }

  try {
    if ((await Deno.stat(target)).isFile) {
      return await fileResponse(target, pathname.startsWith("/assets/"));
    }
  } catch {
    // not a file — fall through
  }

  // A missing file-looking path (last segment has an extension) is a real 404.
  if ((pathname.split("/").pop() ?? "").includes(".")) {
    return new Response(JSON.stringify({ error: "not found" }), {
      status: 404,
      headers: { "content-type": "application/json" },
    });
  }

  // Client route: serve the SPA shell, or a placeholder if the build is absent.
  const index = resolve(root, "index.html");
  try {
    if ((await Deno.stat(index)).isFile) return await fileResponse(index, false);
  } catch {
    // no build present
  }
  warnMissingBuildOnce(root);
  return new Response(PLACEHOLDER, {
    status: 200,
    headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-cache" },
  });
}
