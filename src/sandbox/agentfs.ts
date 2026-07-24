/**
 * agentfs sandbox backend — the copy-on-write overlay that isolates every
 * sandboxed session. Each bough session shares one `agentfs run --session <id>`
 * delta: every invocation overlays the CURRENT WORKING DIR copy-on-write (macOS:
 * NFS mount + sandbox-exec), so writes land in the session's delta and the real
 * tree is untouched. Runs on the same session id JOIN the same overlay, giving
 * read-your-writes across bash + the file tools.
 *
 * This is the ONLY sandbox backend. It exposes the seam the tools consume —
 * ensure/execCommand/execIn/readFile/writeFile/teardown plus the
 * {@link sandboxAgentfs} gate — so bash.ts and the file tools drive it whenever a
 * turn is sandboxed (on by default; opt out with BOUGH_SANDBOX_AGENTFS=0).
 *
 * Every fs op runs the host `agentfs` CLI with its cwd set to the session origin —
 * the copy-on-write base is whatever dir the run starts in. agentfs presents that
 * dir as an NFS overlay mounted at `~/.agentfs/run/<id>/mnt` and cds the sandboxed
 * process INTO the mount, so only paths resolved through the cwd (i.e. RELATIVE to
 * the origin) land in the delta; an absolute host path bypasses the mount and hits
 * the real tree. So callers pass host paths (resolveInWorkspace) and the helpers
 * rewrite any absolute-under-origin path to origin-relative before the run
 * (see {@link overlayPath}).
 *
 * `agentfs run` prints a "Welcome to AgentFS!" session banner to STDERR at the end
 * of every invocation (stdout stays clean). {@link stripBanner} drops it from
 * captured stderr, and {@link execCommand} suppresses it at the source for the
 * streaming bash path.
 *
 * There is no network confinement here (agentfs sandboxes the filesystem only);
 * subprocess traffic goes direct and tools use host credentials.
 */

import { dirname, isAbsolute, relative } from "node:path";

/** The agentfs binary: `$BOUGH_AGENTFS_BIN`, else `agentfs` on PATH. Exported so
 *  the diff/ship substrate (vcs/agentdiff.ts) drives the same CLI. */
export function bin(): string {
  return Deno.env.get("BOUGH_AGENTFS_BIN") ?? "agentfs";
}

/** Whether the agentfs sandbox backend is active. On by default; opt out with
 *  BOUGH_SANDBOX_AGENTFS=0 (a sandboxed turn then runs bash UNSANDBOXED on the host). */
export function sandboxAgentfs(): boolean {
  return Deno.env.get("BOUGH_SANDBOX_AGENTFS") !== "0";
}

export interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

export interface ExecOpts {
  /** Host cwd for the run (the copy-on-write base). Defaults to the session's
   *  ensured origin. */
  cwd?: string;
  /** Extra env for the run, merged onto the host env the sandboxed child
   *  inherits. */
  env?: Record<string, string>;
  /** The turn's interrupt — kills the host `agentfs run` child on abort. */
  signal?: AbortSignal;
}

const dec = new TextDecoder();
const enc = new TextEncoder();

interface Handle {
  /** The session's ORIGIN dir — the overlay's copy-on-write base (cwd of runs). */
  origin: string;
}
const live = new Map<string, Handle>();

/**
 * Record the session's origin so later fs ops overlay the right dir. Cheap: there
 * is no machine to boot — the first `agentfs run` creates the delta lazily, and
 * subsequent runs join it. Always records the CURRENT origin (not first-wins): a
 * session's on-disk base can relocate within its lifetime (first turn moves a repo
 * session into its shadow worktree), and pinning the first origin would leave the
 * overlay pointed at a dir that no longer exists — every fs op then dies with a
 * "No such cwd" from the agentfs backend. The delta is keyed by session id, not
 * origin, so refreshing the base cwd is safe.
 */
export function ensure(sessionId: string, opts: { origin: string }): void {
  live.set(sessionId, { origin: opts.origin });
}

/** Whether the session has an origin recorded in THIS process. */
export function has(sessionId: string): boolean {
  return live.has(sessionId);
}

function originOf(sessionId: string): string {
  const h = live.get(sessionId);
  if (!h) throw new Error(`agentfs session ${sessionId} not ensured`);
  return h.origin;
}

/**
 * Rewrite an absolute host path that sits under `origin` to a path relative to it,
 * so it resolves through the overlay mount (runs cwd into the mount) instead of
 * bypassing it to the real tree. Paths already relative, or absolute but outside
 * the origin, pass through unchanged (a caller confining to the workspace won't hit
 * the latter). A path equal to the origin collapses to ".".
 */
function overlayPath(origin: string, path: string): string {
  if (!isAbsolute(path)) return path;
  const rel = relative(origin, path);
  return rel === "" ? "." : rel.startsWith("..") ? path : rel;
}

/**
 * The host argv that runs `argv` inside the session's overlay for the STREAMING
 * bash path (bash.ts spawns it with its own child/kill/background machinery, cwd
 * set to the session origin). The wrapper:
 *   - drops agentfs's own stderr (the trailing banner) with `2>/dev/null`;
 *   - merges the inner command's stderr into stdout (`exec 2>&1`) so nothing the
 *     command wrote is lost with the banner;
 *   - passes `argv` as positional args to the inner shell, so no quoting or
 *     escaping of the command is needed.
 * The inner command's exit code propagates out through `exec` at both levels.
 */
export function execCommand(sessionId: string, argv: string[]): string[] {
  // Inner shell ($0=bough-inner, $1..=argv): merge stderr, then exec the argv.
  const inner = 'exec 2>&1; exec "$@"';
  // Outer shell: capture bin/session/inner from the head, shift them off, then
  // exec agentfs with argv as the remaining positionals; drop agentfs's stderr.
  const outer =
    'b=$1; s=$2; i=$3; shift 3; exec "$b" run --session "$s" -- /bin/sh -c "$i" bough-inner "$@" 2>/dev/null';
  return ["/bin/sh", "-c", outer, "bough", bin(), sessionId, inner, ...argv];
}

/**
 * Run `argv` in the session's overlay and capture the result. Non-zero exit is
 * data (like bash.ts), not a thrown error. The banner is stripped from stderr.
 * Caller must have {@link ensure}d the session (or pass `opts.cwd`).
 */
export async function execIn(
  sessionId: string,
  argv: string[],
  opts?: ExecOpts,
): Promise<ExecResult> {
  const cwd = opts?.cwd ?? originOf(sessionId);
  const { code, stdout, stderr } = await new Deno.Command(bin(), {
    args: ["run", "--session", sessionId, "--", ...argv],
    cwd,
    env: opts?.env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
    signal: opts?.signal,
  }).output();
  return { code, stdout: dec.decode(stdout), stderr: stripBanner(dec.decode(stderr)) };
}

/**
 * Read a file from the session's overlay into bytes. Streams `cat` over stdout,
 * which stays clean (the banner is on stderr) — so this is binary-safe with no
 * base64 round-trip. Host-path confinement stays the caller's job.
 */
export async function readFile(sessionId: string, path: string): Promise<Uint8Array> {
  const cwd = originOf(sessionId);
  const rel = overlayPath(cwd, path);
  const child = new Deno.Command(bin(), {
    args: ["run", "--session", sessionId, "--", "/bin/cat", rel],
    cwd,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const [out, err, status] = await Promise.all([
    new Response(child.stdout).arrayBuffer(),
    new Response(child.stderr).text(),
    child.status,
  ]);
  if (status.code !== 0) {
    throw new Error(`readFile ${path} failed (${status.code}): ${stripBanner(err).trim()}`);
  }
  return new Uint8Array(out);
}

/**
 * Write bytes (or a UTF-8 string) to a file in the session's overlay, streaming
 * the payload through the command's stdin (`cat >`) — binary-safe, no ARG_MAX
 * limit, no base64. Creates parent directories (in the overlay) first.
 */
export async function writeFile(
  sessionId: string,
  path: string,
  data: Uint8Array | string,
): Promise<void> {
  const cwd = originOf(sessionId);
  const rel = overlayPath(cwd, path);
  const bytes = typeof data === "string" ? enc.encode(data) : data;
  const child = new Deno.Command(bin(), {
    args: [
      "run",
      "--session",
      sessionId,
      "--",
      "/bin/sh",
      "-c",
      `mkdir -p ${shq(dirname(rel))} && cat > ${shq(rel)}`,
    ],
    cwd,
    stdin: "piped",
    stdout: "null",
    stderr: "piped",
  }).spawn();
  const w = child.stdin.getWriter();
  await w.write(bytes);
  await w.close();
  const err = await new Response(child.stderr).text();
  const status = await child.status;
  if (status.code !== 0) {
    throw new Error(`writeFile ${path} failed (${status.code}): ${stripBanner(err).trim()}`);
  }
}

/**
 * Drop the session's in-process handle (session archived). The on-disk delta
 * persists — phase 5 reads it with `agentfs diff` and cleans it up afterward; a
 * lingering delta is harmless. Async so the teardown seam can await it.
 */
export function teardown(sessionId: string): Promise<void> {
  live.delete(sessionId);
  return Promise.resolve();
}

/**
 * Drop agentfs's trailing "Welcome to AgentFS!" session banner (printed to stderr
 * at the end of every `agentfs run`). Anchored on the banner's fixed opening lines
 * so ordinary command output is never truncated.
 */
export function stripBanner(text: string): string {
  return text.replace(
    /(^|\n)Welcome to AgentFS!\n\nThe following directories are writable:[\s\S]*$/,
    "",
  );
}

/** Single-quote a string for `sh -c` (wrap, and escape embedded quotes). */
function shq(s: string): string {
  return `'${s.replaceAll("'", `'\\''`)}'`;
}
