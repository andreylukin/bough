/**
 * Artifacts — files the agent publishes for browser viewing, hosted on the same
 * :4321 origin as the API and web UI. The supervisor's `artifact()` host function
 * writes here (server/../turn.ts wires it); `GET /artifacts/:id/*` serves them; the
 * web UI lists a session's artifacts and links out to open them.
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

/** An artifact the agent published (mirrored by web/src/types.ts `Artifact`). */
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
export function serverBaseUrl(): string {
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
 * Creates parent dirs; overwrites an existing artifact of the same name.
 */
export async function publishArtifact(
  sessionId: string,
  name: string,
  content: string,
  base?: string,
): Promise<Artifact> {
  const rel = name.replace(/^\/+/, "");
  const full = resolveArtifact(sessionId, rel, base);
  await Deno.mkdir(dirname(full), { recursive: true });
  await Deno.writeTextFile(full, content);
  const info = await Deno.stat(full);
  return toArtifact(sessionId, rel, info.size, info.mtime?.getTime() ?? Date.now());
}

/** Every artifact a session has published, newest first. Absent dir → []. */
export async function listArtifacts(sessionId: string, base?: string): Promise<Artifact[]> {
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
 */
export async function serveArtifact(
  sessionId: string,
  name: string,
  base?: string,
): Promise<Response> {
  let full: string;
  try {
    full = resolveArtifact(sessionId, name, base);
  } catch {
    return new Response("forbidden", { status: 403 });
  }
  try {
    if (!(await Deno.stat(full)).isFile) throw new Error("not a file");
    const body = await Deno.readFile(full);
    return new Response(body, {
      headers: { "content-type": contentType(full), "cache-control": "no-cache" },
    });
  } catch {
    return new Response(JSON.stringify({ error: "not found" }), {
      status: 404,
      headers: { "content-type": "application/json" },
    });
  }
}
