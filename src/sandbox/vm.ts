/**
 * VM-session lifecycle for the smolvm sandbox backend.
 *
 * Each bough session gets one persistent smolvm machine, booted from an unpacked
 * golden rootfs DIRECTORY (never a `.smolmachine` pack — the packed `--from` path
 * silently drops `--allow-cidr`). The machine runs a `sleep infinity` workload so
 * it stays up across `exec` calls: local-dir images have no entrypoint, so without
 * a command `start` fails "no command given". Commands, file I/O, and status all
 * shell out to the `smolvm` CLI via `Deno.Command` — the same pattern vcs/shadow.ts
 * uses for git.
 *
 * Egress is locked down at boot by `--allow-cidr <host>/32`: the guest can reach the
 * host gateway (proxy) at that IP and nothing else (all other egress is refused). The
 * proxy/CA env the gateway mints per turn (net/gateway.ts envFor) is injected at
 * `exec` time via the `env` map — nothing here is hardcoded.
 *
 * There is exactly one backend, so this module wraps it directly — no plugin
 * indirection, no speculative abstraction.
 */

/** The smolvm binary: `$BOUGH_SMOLVM_BIN`, else `smolvm` on PATH. */
function bin(): string {
  return Deno.env.get("BOUGH_SMOLVM_BIN") ?? "smolvm";
}

/** A host→guest bind mount; `ro` makes guest writes fail with EROFS. */
export interface Mount {
  host: string;
  guest: string;
  ro?: boolean;
}

export interface CreateOpts {
  /** Session id — becomes the machine name (callers prefix as they see fit). */
  sid: string;
  /** Absolute path to the unpacked golden rootfs directory (the `--image`). */
  goldenDir: string;
  /** Host gateway IP the guest may reach; `/32` is appended for the allow-cidr. */
  gateCidr: string;
  mounts: Mount[];
  cpus?: number;
  /** Memory in MiB. */
  mem?: number;
  /** Baked-in env for the persistent workload (`-e KEY=VALUE`). Per-turn proxy/CA
   *  vars go on `exec` instead — they change every turn. */
  env?: Record<string, string>;
  /** Storage disk size in GiB (`--storage`) — the guest's persistent /dev/vda,
   *  where guest-owned clones live. smolvm's default (20G) when unset. */
  storageGiB?: number;
}

export interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

export interface ExecOpts {
  /** Working directory inside the guest (`-w`). */
  cwd?: string;
  /** Env injected for this call (`-e KEY=VALUE`) — the gateway's proxy/CA map. */
  env?: Record<string, string>;
  /** When set, run with `--stream` and deliver each output line live as it
   *  arrives (the long-running / background variant, mirroring ctx.onLog). The
   *  promise still resolves with the full accumulated result. */
  stream?: (line: string) => void;
  /** The turn's interrupt. On abort we kill the host `machine exec` child (which
   *  detaches the guest command); mirrors bash.ts threading ctx.signal. */
  signal?: AbortSignal;
}

const dec = new TextDecoder();
const enc = new TextEncoder();

/** Run the smolvm CLI, capturing output. Never throws on non-zero exit — the
 *  caller decides (a failed guest command is data, like a non-zero shell exit). */
async function cli(args: string[], signal?: AbortSignal): Promise<ExecResult> {
  const { code, stdout, stderr } = await new Deno.Command(bin(), {
    args,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
    signal,
  }).output();
  return { code, stdout: dec.decode(stdout), stderr: dec.decode(stderr) };
}

/** Run the CLI and throw on non-zero exit — for lifecycle ops that must succeed. */
async function cliOk(args: string[]): Promise<string> {
  const r = await cli(args);
  if (r.code !== 0) {
    throw new Error(`smolvm ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/** Flatten an env map into repeated `-e KEY=VALUE` flags. */
function envFlags(env?: Record<string, string>): string[] {
  if (!env) return [];
  return Object.entries(env).flatMap(([k, v]) => ["-e", `${k}=${v}`]);
}

/**
 * Create the session's machine with the confirmed working spec and start it.
 *
 *   smolvm machine create --name <sid> --image <goldenDir>/ --allow-cidr <ip>/32 \
 *     --volume HOST:GUEST[:ro] ... [-e ...] [--cpus N] [--mem MiB] \
 *     -- /bin/sh -c "sleep infinity"
 *
 * The `sleep infinity` workload is REQUIRED (see module header). `create` registers
 * the machine; `start` boots it. Idempotent-ish: a create over an existing name
 * throws, so callers should `remove` first on reuse.
 */
export async function createSession(opts: CreateOpts): Promise<void> {
  const image = opts.goldenDir.endsWith("/") ? opts.goldenDir : opts.goldenDir + "/";
  const args = [
    "machine",
    "create",
    "--name",
    opts.sid,
    "--image",
    image,
    "--allow-cidr",
    `${opts.gateCidr}/32`,
  ];
  for (const m of opts.mounts) {
    args.push("--volume", `${m.host}:${m.guest}${m.ro ? ":ro" : ""}`);
  }
  if (opts.cpus !== undefined) args.push("--cpus", String(opts.cpus));
  if (opts.mem !== undefined) args.push("--mem", String(opts.mem));
  if (opts.storageGiB !== undefined) args.push("--storage", String(opts.storageGiB));
  args.push(...envFlags(opts.env));
  args.push("--", "/bin/sh", "-c", "sleep infinity");
  await cliOk(args);
  await cliOk(["machine", "start", "--name", opts.sid]);
}

/**
 * Run `argv` inside the session's guest via `machine exec`. Returns the guest
 * command's exit code and captured output — a non-zero code is normal data, not a
 * thrown error (same contract as bash.ts). Pass `opts.stream` for the long-running
 * variant: output lines are delivered live as they arrive, and the promise still
 * resolves with the full result.
 */
export async function exec(
  sid: string,
  argv: string[],
  opts?: ExecOpts,
): Promise<ExecResult> {
  const pre = execArgs(sid, argv, opts);
  if (opts?.stream) pre.push("--stream");
  if (!opts?.stream) return await cli(pre, opts?.signal);
  return await execStreaming(pre, opts.stream, opts?.signal);
}

/** The `machine exec` argument vector (no `--stream`) for running `argv` in `sid`.
 *  Callers that spawn the child themselves (bash.ts reuses its own streaming/bg/
 *  kill machinery) prepend {@link smolvmBin}. */
export function execArgs(sid: string, argv: string[], opts?: ExecOpts): string[] {
  const pre = ["machine", "exec", "--name", sid];
  if (opts?.cwd) pre.push("-w", opts.cwd);
  pre.push(...envFlags(opts?.env));
  pre.push("--", ...argv);
  return pre;
}

/** The resolved smolvm binary (for callers building their own child process). */
export function smolvmBin(): string {
  return bin();
}

/** Spawn `machine exec --stream`, forward each stdout line to `onLine`, and
 *  accumulate the full stdout/stderr for the resolved result. */
async function execStreaming(
  args: string[],
  onLine: (line: string) => void,
  signal?: AbortSignal,
): Promise<ExecResult> {
  const child = new Deno.Command(bin(), {
    args,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
    signal,
  }).spawn();

  const outChunks: string[] = [];
  const pumpOut = (async () => {
    let carry = "";
    for await (const chunk of child.stdout.pipeThrough(new TextDecoderStream())) {
      outChunks.push(chunk);
      carry += chunk;
      const lines = carry.split("\n");
      carry = lines.pop() ?? "";
      for (const line of lines) onLine(line);
    }
    if (carry) onLine(carry);
  })();

  const errChunks: string[] = [];
  const pumpErr = (async () => {
    for await (const chunk of child.stderr.pipeThrough(new TextDecoderStream())) {
      errChunks.push(chunk);
    }
  })();

  const [status] = await Promise.all([child.status, pumpOut, pumpErr]);
  return { code: status.code, stdout: outChunks.join(""), stderr: errChunks.join("") };
}

/**
 * Read a guest file into bytes, via a base64 round-trip over `exec` (busybox
 * `base64` encodes by default). This is the seam the in-process file tools route
 * through — guest-path confinement stays the caller's job.
 */
export async function readFile(sid: string, path: string): Promise<Uint8Array> {
  const r = await exec(sid, ["/bin/sh", "-c", `base64 ${shq(path)}`]);
  if (r.code !== 0) {
    throw new Error(`readFile ${path} failed (${r.code}): ${r.stderr.trim()}`);
  }
  // Deno's atob wants no embedded newlines; busybox wraps at 76 cols.
  return Uint8Array.from(atob(r.stdout.replace(/\s+/g, "")), (c) => c.charCodeAt(0));
}

/** base64 without spreading the byte array (a spread blows the call stack on
 *  large inputs) and without embedded newlines. */
function b64encode(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** Max base64 chars per `machine exec` call — bounded well under ARG_MAX so the
 *  embedded-in-argv command never overflows; larger files stream in chunks. */
const WRITE_CHUNK = 96 * 1024;

/**
 * Write bytes (or a UTF-8 string) to a guest file, base64-embedded in the command
 * (no stdin plumbing through `machine exec`). Large payloads are split into
 * ARG_MAX-safe chunks: the first truncates the target, the rest append.
 */
export async function writeFile(
  sid: string,
  path: string,
  data: Uint8Array | string,
): Promise<void> {
  const b64 = b64encode(typeof data === "string" ? enc.encode(data) : data);
  const qp = shq(path);
  for (let off = 0, first = true; off < b64.length || first; off += WRITE_CHUNK, first = false) {
    const part = b64.slice(off, off + WRITE_CHUNK);
    const redir = off === 0 ? ">" : ">>";
    const r = await exec(sid, [
      "/bin/sh",
      "-c",
      `printf %s ${shq(part)} | base64 -d ${redir} ${qp}`,
    ]);
    if (r.code !== 0) {
      throw new Error(`writeFile ${path} failed (${r.code}): ${r.stderr.trim()}`);
    }
  }
}

/** Single-quote a string for `sh -c` (wrap, and escape embedded quotes). */
function shq(s: string): string {
  return `'${s.replaceAll("'", `'\\''`)}'`;
}

/** Stop the session's machine (state persists; `start` resumes). */
export async function stop(sid: string): Promise<void> {
  await cliOk(["machine", "stop", "--name", sid]);
}

/** Start (boot) a previously-created machine. */
export async function start(sid: string): Promise<void> {
  await cliOk(["machine", "start", "--name", sid]);
}

/** Delete the machine and its state (force-skips the confirm prompt). */
export async function remove(sid: string): Promise<void> {
  await cliOk(["machine", "delete", "--name", sid, "--force"]);
}

/** Status record for one machine, or null if it doesn't exist. */
export async function status(sid: string): Promise<Record<string, unknown> | null> {
  const r = await cli(["machine", "status", "--name", sid, "--json"]);
  if (r.code !== 0) return null;
  try {
    return JSON.parse(r.stdout) as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** All machines smolvm knows about (parsed `machine ls --json`). */
export async function list(): Promise<Record<string, unknown>[]> {
  const out = await cliOk(["machine", "ls", "--json"]);
  return JSON.parse(out) as Record<string, unknown>[];
}

/**
 * TODO(unverified): fork a running machine into a child session (CoW memory +
 * disks) via `smolvm machine fork`. Fork is proven to give process/memory
 * isolation, but whether the child correctly INHERITS the parent's egress
 * lockdown (`--allow-cidr`) and its `--volume` mounts is UNVERIFIED — do not
 * rely on it for isolation guarantees until that is tested. The parent must have
 * been created forkable. Left as a typed stub deliberately.
 */
export function fork(_sid: string, _childSid: string): Promise<void> {
  return Promise.reject(
    new Error("fork() is an unverified stub: egress + mount inheritance not yet confirmed"),
  );
}
