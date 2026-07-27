/**
 * Serving artifacts: content types, the comment layer, and the two routes.
 *
 * The store itself — where artifacts live, and the confinement rules for names and
 * session ids — is `hostfn/artifact.ts`, because `hostfn/` may not import from
 * `server/` (plan §3) and the confinement rules must exist exactly once. This file is
 * the HTTP half: it reads what the store resolved and turns it into a `Response`.
 *
 * THE INVARIANT THIS HOLDS: **every served HTML artifact gets the comment layer
 * injected AT SERVE TIME** (`comments.ts`), and nothing else does. Two consequences,
 * both deliberate:
 *
 *   - The bytes on disk stay exactly what the agent wrote. A page the user saves or
 *     forwards is the page, not the page plus an annotation toolbar pointed at a
 *     loopback server that is not running.
 *   - The layer only exists where it means something, which is inside bough.
 *
 * It goes into HTML documents only. Injecting into the page's own CSS or JS — served
 * through the same route — would corrupt them, and the layer would not work anyway.
 *
 * TRAVERSAL IS A 403, NOT A 404. "That path is not addressable" and "nothing is
 * there" are different facts, and collapsing them sends whoever is debugging to the
 * wrong place — a mistyped session id reads as a deleted artifact.
 *
 * A 404 that a BROWSER asked for is an HTML page, not a JSON body. Artifact links get
 * opened by the audience artifacts exist for, who are not reading `{"error":"not
 * found"}` in a tab; a replaced or mistyped link deserves a sentence saying what
 * happened.
 *
 * Trust note, stated rather than implied: artifacts are agent-authored HTML/JS served
 * same-origin, so an opened artifact runs with this origin's privileges. That is
 * deliberate — it is explicit agent OUTPUT the user chooses to open, not a containment
 * boundary. Treat an artifact like any other file the agent wrote (spec §11).
 *
 * Ported from `src/server/artifacts.ts`. Deltas are marked `NOTE:`.
 */
import { listArtifacts, resolveArtifactPath } from "../hostfn/artifact.ts";
import type { ArtifactStoreOptions } from "../hostfn/artifact.ts";
import { type Handler, json } from "./app.ts";
import { commentWidget } from "./comments.ts";

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".htm": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
  ".ico": "image/x-icon",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
  ".map": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".md": "text/plain; charset=utf-8",
  ".csv": "text/csv; charset=utf-8",
  ".wasm": "application/wasm",
};

/** The declared content type for a path, or octet-stream when nothing matches. */
export function contentTypeFor(path: string): string {
  const dot = path.lastIndexOf(".");
  return (dot >= 0 && MIME[path.slice(dot).toLowerCase()]) || "application/octet-stream";
}

/**
 * Agents publish HTML under a bare name (`my-explorer`) often enough to matter, and
 * an octet-stream response makes the browser download it instead of rendering it —
 * the user clicks the link the agent gave them and gets a file in ~/Downloads. So for
 * an EXTENSIONLESS file only, sniff the first bytes: leading markup → HTML.
 */
async function sniffHtml(full: string): Promise<string | null> {
  const head = new Uint8Array(64);
  let n: number | null = 0;
  const file = await Deno.open(full);
  try {
    n = await file.read(head);
  } finally {
    file.close();
  }
  return new TextDecoder().decode(head.subarray(0, n ?? 0)).trimStart().startsWith("<")
    ? "text/html; charset=utf-8"
    : null;
}

function basename(path: string): string {
  const cut = path.lastIndexOf("/");
  return cut >= 0 ? path.slice(cut + 1) : path;
}

// ---------------------------------------------------------------------------
// Serve
// ---------------------------------------------------------------------------

/**
 * Splice the comment layer in before `</body>`, or append it when the document has no
 * body tag (a fragment still renders, and the layer still works).
 */
export function injectCommentLayer(html: string): string {
  const widget = commentWidget();
  const idx = html.toLowerCase().lastIndexOf("</body>");
  return idx >= 0 ? html.slice(0, idx) + widget + html.slice(idx) : html + widget;
}

/**
 * The browser-facing 404. Self-contained, no external anything — the same bar the
 * artifacts themselves are held to (spec §11).
 */
export const NOT_FOUND_PAGE = `<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>This page isn't here</title>
<style>
:root { color-scheme: light dark; }
body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
  background: #fcfcfb; color: #0b0b0b; font: 14px/1.55 system-ui, sans-serif; }
main { max-width: 34em; padding: 32px 28px; }
.eyebrow { font: 600 11.5px ui-monospace, Menlo, monospace; text-transform: uppercase;
  letter-spacing: 0.08em; color: #52514e; margin: 0 0 14px; }
h1 { font-size: 21px; font-weight: 650; letter-spacing: -0.01em; margin: 0 0 10px; }
p { margin: 0; color: #52514e; }
@media (prefers-color-scheme: dark) {
  body { background: #1a1a19; color: #f4f3ef; }
  .eyebrow, p { color: #c3c2b7; }
}
</style>
<main>
<p class="eyebrow">404 &middot; not found</p>
<h1>This page isn't here</h1>
<p>It may have moved or been replaced. Ask bough to share it again.</p>
</main>
`;

export interface ServeArtifactOptions extends ArtifactStoreOptions {
  /** The request's `Accept` header — a browser gets the HTML 404, a client the JSON. */
  accept?: string;
}

/**
 * Serve one artifact file.
 *
 * `no-cache` because artifacts are overwritten in place: a cached stale page is
 * indistinguishable from an agent that did nothing, and republishing is the normal
 * way a program iterates.
 *
 * NOTE: the port also rendered `*.ui.json` spec artifacts through a bundled viewer
 * (`jsonrender/`). That subsystem is not in the rewrite's layout (plan §3) and is not
 * ported; a `.ui.json` artifact serves as JSON like any other file.
 */
export async function serveArtifact(
  sessionId: string,
  name: string,
  opts: ServeArtifactOptions = {},
): Promise<Response> {
  let full: string;
  try {
    full = resolveArtifactPath(sessionId, name, opts);
  } catch {
    return new Response("forbidden", {
      status: 403,
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  }

  try {
    if (!(await Deno.stat(full)).isFile) throw new Error("not a file");

    let type = contentTypeFor(full);
    if (type === "application/octet-stream" && !basename(full).includes(".")) {
      type = (await sniffHtml(full)) ?? type;
    }

    if (type.startsWith("text/html")) {
      const html = await Deno.readTextFile(full);
      return new Response(injectCommentLayer(html), {
        headers: { "content-type": type, "cache-control": "no-cache" },
      });
    }

    return new Response(await Deno.readFile(full), {
      headers: { "content-type": type, "cache-control": "no-cache" },
    });
  } catch {
    if (opts.accept?.includes("text/html")) {
      return new Response(NOT_FOUND_PAGE, {
        status: 404,
        headers: { "content-type": "text/html; charset=utf-8" },
      });
    }
    return new Response(JSON.stringify({ error: `no artifact ${name} for session ${sessionId}` }), {
      status: 404,
      headers: { "content-type": "application/json; charset=utf-8" },
    });
  }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/**
 * `GET /sessions/:id/artifacts` — what this session has published.
 *
 * Answered from the filesystem, so it is correct for a session whose row is gone and
 * for artifacts published by a previous process (spec §4). It deliberately does NOT
 * check that the session exists: the artifacts outlive the row, and 404-ing here
 * would hide files that are demonstrably on disk.
 */
export const listArtifactsH: Handler = (_req, _ctx, params) =>
  json({ artifacts: listArtifacts(decodeSegments(params.id)) });

/**
 * `GET /artifacts/:id/:path*` — the hosted file itself.
 *
 * Same origin as the API on purpose: a link the agent prints is a link the user's
 * browser opens with no extra machinery, and the injected comment layer can talk back
 * to `/sessions/:id/comments` without CORS.
 */
export const getArtifactH: Handler = (req, _ctx, params) =>
  serveArtifact(decodeSegments(params.id), decodeSegments(params.path ?? ""), {
    accept: req.headers.get("accept") ?? undefined,
  });

/**
 * Percent-decode a matched path, segment by segment.
 *
 * `URLPattern` hands back the raw pathname and the store encodes each segment when it
 * builds `url`, so a name with a space round-trips only if it is decoded here. Per
 * segment, not whole: decoding the whole string would turn an encoded `%2F` inside one
 * segment into a real separator, which is a traversal primitive. A malformed escape
 * decodes to itself and then fails confinement or the stat, rather than throwing a
 * `URIError` out of a handler.
 */
function decodeSegments(path: string): string {
  return path.split("/").map((seg) => {
    try {
      return decodeURIComponent(seg);
    } catch {
      return seg;
    }
  }).join("/");
}
