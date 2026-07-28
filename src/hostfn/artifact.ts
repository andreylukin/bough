/**
 * `artifact(name, content)` — how a program hands the user something to LOOK at in a
 * browser — and the per-session store underneath it.
 *
 * TWO INVARIANTS.
 *
 * **1. Publishing never touches the workspace.** The bytes go to
 * `~/.bough/artifacts/<sessionId>/`, outside the checkout, so the diff the user
 * reviews stays the work and nothing else (spec §11). A program that wanted this
 * effect without the verb would `write("report.html", …)` into the repo and drop a
 * generated page into `git status` — exactly the pollution the store exists to
 * prevent, and the reason this is a host function rather than advice in the prompt.
 *
 * **2. Names and session ids are CONFINED to their directory.** Both arrive from
 * outside — the name from a program's call, the session id from a URL someone can
 * type — and both are used to build a path the *server* then reads or writes.
 * `confine()` (paths.ts) rejects `..`, absolute paths, and segment chains that
 * resolve outward; a session id must additionally be a single path segment, so one
 * session cannot address another's directory by spelling it `a/../b`.
 *
 * To be plain about what that is: not a sandbox. Programs run as the user with the
 * user's full authority and can read or write any path they like (spec §2).
 * Confinement guards the *server's* own path construction, so `GET
 * /artifacts/<id>/<name>` cannot be steered into `~/.ssh`, and one session's publish
 * cannot land in another's listing.
 *
 * **The filesystem is the source of truth.** There is no artifacts table and no row
 * to keep in sync: `listArtifacts` walks the directory, so a listing survives a
 * database reset, a fresh `bough.db`, or a server that has never seen the session
 * (spec §4). A store that needed a row would report nothing for artifacts sitting
 * right there on disk.
 *
 * WHY THE STORE LIVES HERE and not in `server/`: `hostfn/` may not import from
 * `server/` (plan §3), and the confinement rules must exist exactly once — a store
 * and a server that disagree about which names are legal is a traversal bug waiting
 * for one of them to be updated alone. So the primitives are here, taking plain
 * parameters and no HTTP, and `server/artifacts.ts` imports them to serve and to
 * list over the wire. That is the same direction `server/questions.ts` →
 * `hostfn/ask.ts` already runs.
 *
 * Ported from `src/server/artifacts.ts`. Deltas are marked `NOTE:`.
 */
import { type Dirent, readdirSync, type Stats, statSync } from "node:fs";
import { mkdir, stat as statFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { ArtifactError, PathError } from "../errors.ts";
import { artifactsDir, confine } from "../paths.ts";
import type { HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** One published file. */
export interface Artifact {
  /** Session-relative path, forward-slashed (`index.html`, `assets/app.js`). */
  name: string;
  /** Same-origin path the UI links to: `/artifacts/<sessionId>/<name>`. */
  url: string;
  /** Absolute loopback URL — what the agent prints for the user to click. */
  href: string;
  bytes: number;
  /** Publish/update time (mtime epoch ms). */
  ts: number;
}

/**
 * Where the store lives and what its links look like.
 *
 * Injected rather than read from the environment at each call site, per the
 * dependency-injection ground rule: a test points `root` at a temp directory and gets
 * a hermetic store, with no `BOUGH_HOME` mutation and nothing written under the real
 * `~/.bough`.
 */
export interface ArtifactStoreOptions {
  /** The artifacts root. Absent = `~/.bough/artifacts` (`paths.ts`). */
  root?: string;
  /** Origin for `href`. Absent = the loopback base this server is reachable at. */
  baseUrl?: string;
}

// ---------------------------------------------------------------------------
// Paths — the confinement rules
// ---------------------------------------------------------------------------

/**
 * The loopback base URL this server is reachable at.
 *
 * Always 127.0.0.1: the server binds loopback and only loopback (spec §17), and
 * `href` is what the LOCAL user clicks. The UI links the relative `url` and is
 * origin-agnostic either way.
 */
export function serverBaseUrl(): string {
  return `http://127.0.0.1:${process.env.BOUGH_PORT ?? "4321"}`;
}

/**
 * One session's artifact directory, or a `PathError` when the id is not a single
 * confined segment.
 *
 * `confine` alone rejects `..` and absolute ids; the parent check is what rejects a
 * *descending* id like `other/nested`, which stays inside the root but addresses a
 * directory that is not its own — and would let two different id strings name the
 * same directory.
 */
export function sessionArtifactDir(sessionId: string, opts: ArtifactStoreOptions = {}): string {
  const root = resolve(opts.root ?? artifactsDir());
  if (!sessionId) {
    throw new PathError("artifact session id is empty — name the session that published it.");
  }
  const dir = confine(root, sessionId);
  if (dir === root || dirname(dir) !== root) {
    throw new PathError(
      `artifact session id must be one path segment: ${JSON.stringify(sessionId)} resolves ` +
        `to ${dir}, which is not a direct child of ${root}.`,
    );
  }
  return dir;
}

/**
 * Resolve `name` under the session's directory. Throws `PathError` on anything that
 * escapes, and on a name that resolves to the directory itself.
 *
 * Leading slashes are stripped rather than rejected: `/index.html` from a URL path,
 * or from a program that wrote an absolute-looking name, means the store's own root,
 * and reading it that way is what every caller intends. Everything after that is
 * confined for real.
 */
export function resolveArtifactPath(
  sessionId: string,
  name: string,
  opts: ArtifactStoreOptions = {},
): string {
  const dir = sessionArtifactDir(sessionId, opts);
  const rel = name.replace(/^\/+/, "");
  if (!rel) {
    throw new PathError(
      "artifact name is empty — publish under a plain relative name like index.html.",
    );
  }
  const full = confine(dir, rel);
  if (full === dir) {
    throw new PathError(
      `artifact name ${JSON.stringify(name)} names the session's directory, not a file.`,
    );
  }
  return full;
}

function toArtifact(
  sessionId: string,
  name: string,
  bytes: number,
  ts: number,
  opts: ArtifactStoreOptions,
): Artifact {
  const url = `/artifacts/${encodeURIComponent(sessionId)}/${
    name.split("/").map(encodeURIComponent).join("/")
  }`;
  return { name, url, href: (opts.baseUrl ?? serverBaseUrl()) + url, bytes, ts };
}

// ---------------------------------------------------------------------------
// Publish and list
// ---------------------------------------------------------------------------

/**
 * Write `content` into the session's store and describe it.
 *
 * Creates parent directories and overwrites an existing artifact of the same name —
 * republishing `index.html` is how a program iterates on a page, and a link the user
 * already has open has to keep working.
 */
export async function publishArtifact(
  sessionId: string,
  name: string,
  content: string,
  opts: ArtifactStoreOptions = {},
): Promise<Artifact> {
  const rel = name.replace(/^\/+/, "");
  const full = resolveArtifactPath(sessionId, rel, opts);
  await mkdir(dirname(full), { recursive: true });
  await writeFile(full, content);
  const info = await statFile(full);
  return toArtifact(sessionId, rel, info.size, info.mtime?.getTime() ?? Date.now(), opts);
}

/**
 * Every artifact a session has published, newest first. An absent directory is an
 * empty list, not an error — a session that never published one is the normal case.
 *
 * This walks the FILESYSTEM and consults no table, which is the source-of-truth rule
 * made operational: drop the database, start a fresh one, and the listing is still
 * right.
 */
export function listArtifacts(sessionId: string, opts: ArtifactStoreOptions = {}): Artifact[] {
  let dir: string;
  try {
    dir = sessionArtifactDir(sessionId, opts);
  } catch {
    return []; // an unaddressable id has published nothing, by construction
  }
  const out: Artifact[] = [];
  const walk = (abs: string, rel: string): void => {
    let entries: Dirent[];
    try {
      entries = readdirSync(abs, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const childRel = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        walk(join(abs, entry.name), childRel);
        continue;
      }
      if (!entry.isFile()) continue;
      let info: Stats;
      try {
        info = statSync(join(abs, entry.name));
      } catch {
        continue; // raced a delete
      }
      out.push(toArtifact(sessionId, childRel, info.size, info.mtime?.getTime() ?? 0, opts));
    }
  };
  walk(dir, "");
  out.sort((a, b) => b.ts - a.ts);
  return out;
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/**
 * Publish with the failure text a MODEL reads.
 *
 * `confine`'s message explains a path escape to a developer; this one tells the next
 * round what to do instead, which is what error text is for (spec §6). A refusal
 * costs a `catch`, not a round.
 */
export async function publishForProgram(
  sessionId: string,
  name: string,
  content: string,
  opts: ArtifactStoreOptions = {},
): Promise<Artifact> {
  try {
    return await publishArtifact(sessionId, name, content, opts);
  } catch (err) {
    if (err instanceof PathError) {
      throw new ArtifactError(
        400,
        `artifact("${name}"): that name escapes this session's artifact directory, and ` +
          `nothing was written. Publish under a plain relative name — "index.html", ` +
          `"assets/app.js" — with no leading slash and no ".." segments.`,
      );
    }
    throw new ArtifactError(
      500,
      `artifact("${name}"): could not be written (${
        err instanceof Error ? err.message : String(err)
      }). Check that the name is a usable filename, then publish again.`,
    );
  }
}

export type ArtifactDeps = ArtifactStoreOptions;

/**
 * Build the bridged `artifact` host function for one turn.
 *
 * Scoped to `ctx.sessionId`, which is the confinement that matters at this layer: a
 * program cannot name another session's store because it never gets to name a session
 * at all. A subagent therefore publishes into its OWN directory and its `href` still
 * works — the store is per-session, not per-tree, and the report it hands back carries
 * the link.
 *
 * The wire is string-only (`harness/protocol.ts`), so the result travels as JSON and
 * the worker re-inflates it before the program sees it.
 */
export function createArtifactHostFn(
  ctx: TurnCtx,
  deps: ArtifactDeps = {},
): Pick<HostFns, "artifact"> {
  return {
    artifact: async (name: string, content: string): Promise<string> => {
      const published = await publishForProgram(ctx.sessionId, name, content ?? "", deps);
      // NOTE: the port returned only `{url, href}`. `name` and `bytes` ride along
      // because a program publishing several files wants to log what it wrote, and a
      // zero-byte artifact is otherwise indistinguishable from a written one.
      return JSON.stringify({
        name: published.name,
        url: published.url,
        href: published.href,
        bytes: published.bytes,
      });
    },
  };
}
