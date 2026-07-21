/**
 * Artifacts — files the agent publishes for browser viewing, hosted on the same
 * :4321 origin as the API. The supervisor's `artifact()` host function
 * writes here (server/../turn.ts wires it); `GET /artifacts/:id/*` serves them;
 * `GET /sessions/:id/artifacts` lists a session's artifacts.
 *
 * Stored under ~/.bough/artifacts/<sessionId>/ — OUTSIDE the workspace, so a published
 * artifact never pollutes the repo diff the user reviews. The filesystem is the source
 * of truth: listing survives a restart with no DB row. Names and session ids are
 * confined to their dir (traversal blocked), so one session can't read or write
 * another's, and nothing escapes ~/.bough.
 *
 * Trust note: artifacts are agent-authored HTML/JS served same-origin, so an opened
 * artifact runs with the page's origin. That is deliberate — this is explicit agent
 * OUTPUT the user chooses to open, distinct from the sandboxed workspace. It is not a
 * containment boundary; treat an artifact like any file the agent wrote.
 */
import { dirname, join, normalize, resolve } from "node:path";
import { commentWidget } from "./comments.ts";
import { validateUiSpec } from "./jsonrender/catalog.ts";
import { viewerPage } from "./jsonrender/bundle.ts";

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

function contentType(path: string): string {
  const dot = path.lastIndexOf(".");
  return (dot >= 0 && MIME[path.slice(dot).toLowerCase()]) || "application/octet-stream";
}

/**
 * Agents sometimes publish an HTML page under a bare name ("my-explorer"), which
 * would otherwise serve as octet-stream and download instead of render. For
 * extensionless files, sniff the head: markup → text/html.
 */
async function sniffedType(full: string): Promise<string | null> {
  const head = new Uint8Array(64);
  const f = await Deno.open(full);
  let n: number | null;
  try {
    n = await f.read(head);
  } finally {
    f.close();
  }
  const text = new TextDecoder().decode(head.subarray(0, n ?? 0)).trimStart();
  return text.startsWith("<") ? "text/html; charset=utf-8" : null;
}

/**
 * `*.ui.json` artifacts are json-render UI specs (jsonrender/catalog.ts): validated
 * against the component catalog at publish time and served as a rendered viewer page.
 */
function isUiSpec(name: string): boolean {
  return name.toLowerCase().endsWith(".ui.json");
}

/** Page title for a spec artifact: the root element's title prop when it has one. */
function specTitle(spec: unknown, fallback: string): string {
  const s = spec as { root?: string; elements?: Record<string, { props?: { title?: unknown } }> };
  const title = s.elements?.[s.root ?? ""]?.props?.title;
  return typeof title === "string" && title ? title : fallback;
}

/**
 * Browser-facing 404. Artifact links are opened by the non-technical audience
 * artifacts exist for, and a mistyped/replaced link answered with raw JSON is a
 * dead end in Chrome — so requests that prefer text/html get a plain-language
 * page in the viewer's footer aesthetic (styles.ts palette, self-contained).
 */
const NOT_FOUND_PAGE = `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>This page isn't here</title>
<style>
:root { color-scheme: light dark; }
body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
  background: #fcfcfb; color: #0b0b0b; font: 14px/1.55 system-ui, sans-serif; }
main { max-width: 34em; padding: 32px 28px; }
.eyebrow { font: 600 11.5px ui-monospace, "SF Mono", Menlo, monospace; text-transform: uppercase;
  letter-spacing: 0.08em; color: #52514e; margin: 0 0 14px; }
h1 { font-size: 21px; font-weight: 650; letter-spacing: -0.01em; margin: 0 0 10px; }
p { margin: 0; color: #52514e; }
@media (prefers-color-scheme: dark) {
  body { background: #1a1a19; color: #f4f3ef; }
  .eyebrow, p { color: #c3c2b7; }
}
</style>
</head>
<body>
<main>
<p class="eyebrow">404 · not found</p>
<h1>This page isn't here</h1>
<p>It may have moved or been replaced. Ask bough to share it again.</p>
</main>
</body>
</html>`;

/** Splice the comment layer in before </body> (or append if there's no body tag). */
function injectCommentWidget(html: string): string {
  const widget = commentWidget();
  const idx = html.toLowerCase().lastIndexOf("</body>");
  return idx >= 0 ? html.slice(0, idx) + widget + html.slice(idx) : html + widget;
}

/** An artifact the agent published. */
export interface Artifact {
  /** Session-relative path, forward-slashed (e.g. "index.html" or "assets/app.js"). */
  name: string;
  /** Same-origin path the UI links to: /artifacts/<sessionId>/<name>. */
  url: string;
  /** Absolute loopback URL for the terminal / the agent's reply. */
  href: string;
  bytes: number;
  /** Publish/update time (mtime epoch ms). */
  ts: number;
}

/** Root for all artifacts: ~/.bough/artifacts (`base` overrides ~/.bough for tests). */
export function artifactsRoot(base?: string): string {
  const home = base ?? join(Deno.env.get("HOME") ?? ".", ".bough");
  return join(home, "artifacts");
}

/**
 * The loopback base URL this server is reachable at. Always 127.0.0.1 even when bound
 * to 0.0.0.0 for LAN/tunnel use: the `href` is what the LOCAL user clicks; the UI uses
 * the relative `url` and so is origin-agnostic.
 */
function serverBaseUrl(): string {
  const port = Deno.env.get("BOUGH_PORT") ?? "4321";
  return `http://127.0.0.1:${port}`;
}

/** The resolved artifact dir for a session; throws on a session id that isn't a plain token. */
function sessionDir(sessionId: string, base?: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) {
    throw new Error(`invalid session id: ${sessionId}`);
  }
  return resolve(join(artifactsRoot(base), sessionId));
}

/** Resolve `name` under the session dir, blocking traversal. Returns the absolute path. */
function resolveArtifact(sessionId: string, name: string, base?: string): string {
  const dir = sessionDir(sessionId, base);
  const rel = name.replace(/^\/+/, "");
  if (!rel) throw new Error("artifact name is empty");
  const full = normalize(resolve(dir, rel));
  if (full !== dir && !full.startsWith(dir + "/")) {
    throw new Error(`artifact name escapes the session dir: ${name}`);
  }
  return full;
}

function toArtifact(sessionId: string, name: string, bytes: number, ts: number): Artifact {
  const url = `/artifacts/${sessionId}/${name.split("/").map(encodeURIComponent).join("/")}`;
  return { name, url, href: serverBaseUrl() + url, bytes, ts };
}

/**
 * Write `content` to the session's artifact store and return its {url, href, …}.
 * Creates parent dirs; overwrites an existing artifact of the same name. A
 * `*.ui.json` spec is validated against the catalog first — an off-catalog spec
 * throws (the message reaches the agent through the artifact() host call), and a
 * valid one is stored in its normalized auto-fixed form.
 */
export async function publishArtifact(
  sessionId: string,
  name: string,
  content: string,
  base?: string,
): Promise<Artifact> {
  const rel = name.replace(/^\/+/, "");
  const full = resolveArtifact(sessionId, rel, base);
  if (isUiSpec(rel)) content = JSON.stringify(validateUiSpec(content));
  await Deno.mkdir(dirname(full), { recursive: true });
  await Deno.writeTextFile(full, content);
  const info = await Deno.stat(full);
  return toArtifact(sessionId, rel, info.size, info.mtime?.getTime() ?? Date.now());
}

/** Every artifact a session has published, newest first. Absent dir → []. */
export function listArtifacts(sessionId: string, base?: string): Artifact[] {
  let dir: string;
  try {
    dir = sessionDir(sessionId, base);
  } catch {
    return [];
  }
  const out: Artifact[] = [];
  const walk = (abs: string, rel: string): void => {
    let entries: Deno.DirEntry[];
    try {
      entries = [...Deno.readDirSync(abs)];
    } catch {
      return;
    }
    for (const e of entries) {
      const childRel = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory) {
        walk(join(abs, e.name), childRel);
      } else if (e.isFile) {
        const info = Deno.statSync(join(abs, e.name));
        out.push(toArtifact(sessionId, childRel, info.size, info.mtime?.getTime() ?? 0));
      }
    }
  };
  walk(dir, "");
  out.sort((a, b) => b.ts - a.ts);
  return out;
}

/**
 * Serve one artifact file. Traversal / bad-id → 403; missing → 404; else the file with
 * its content type and a no-cache header (artifacts are overwritten in place).
 * A `*.ui.json` spec serves as its rendered viewer page (comment layer included);
 * `raw` skips the wrapper and returns the spec JSON itself. `accept` is the
 * request's Accept header: a client that prefers text/html gets an HTML 404,
 * anything else keeps the JSON body.
 */
export async function serveArtifact(
  sessionId: string,
  name: string,
  base?: string,
  opts?: { raw?: boolean; accept?: string },
): Promise<Response> {
  let full: string;
  try {
    full = resolveArtifact(sessionId, name, base);
  } catch {
    return new Response("forbidden", { status: 403 });
  }
  try {
    if (!(await Deno.stat(full)).isFile) throw new Error("not a file");
    if (isUiSpec(full) && !opts?.raw) {
      const specJson = await Deno.readTextFile(full);
      const html = viewerPage(specJson, specTitle(JSON.parse(specJson), name));
      return new Response(injectCommentWidget(html), {
        headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-cache" },
      });
    }
    let type = contentType(full);
    if (
      type === "application/octet-stream" && !full.slice(full.lastIndexOf("/") + 1).includes(".")
    ) {
      type = (await sniffedType(full)) ?? type;
    }
    // Top-level HTML documents get the comment layer injected at serve time
    // (comments.ts) — the on-disk artifact stays clean/portable; the layer only
    // lives where you'd use it (inside bough). Other resources serve as-is.
    if (type.startsWith("text/html")) {
      const html = await Deno.readTextFile(full);
      return new Response(injectCommentWidget(html), {
        headers: { "content-type": type, "cache-control": "no-cache" },
      });
    }
    const body = await Deno.readFile(full);
    return new Response(body, {
      headers: { "content-type": type, "cache-control": "no-cache" },
    });
  } catch {
    if (opts?.accept?.includes("text/html")) {
      return new Response(NOT_FOUND_PAGE, {
        status: 404,
        headers: { "content-type": "text/html; charset=utf-8" },
      });
    }
    return new Response(JSON.stringify({ error: "not found" }), {
      status: 404,
      headers: { "content-type": "application/json" },
    });
  }
}
