/**
 * Symbol navigation: the curated `lsp.*` verbs, and the classifier that decides
 * what a backend invocation MEANT.
 *
 * THE INVARIANT THIS HOLDS: **an empty answer and a broken backend are different
 * outcomes, and this module is the only place that decides which one happened.**
 * Spec §10 and plan §6.14 state the rule from the model's side — "a verb that finds
 * nothing has NOT failed; if the backend itself errors, the program drops to rg +
 * view + patch for the rest of the task and finishes the job" — and every branch
 * below exists to make exactly one of those two sentences true at a time. Collapse
 * them and the product breaks in both directions: a misspelled symbol reported as a
 * backend failure makes the agent abandon symbol navigation for the whole task over
 * a typo, and a dead language server reported as "no results" makes it conclude the
 * symbol does not exist and delete the call sites it could not see.
 *
 * WHY THE NAMES ARE OURS. The verbs are bough's, not the backing tool's. `lsp.refs`
 * is the contract; `leta refs` is an implementation detail behind `VERBS`. This
 * indirection has already paid for itself once — the backend was serena-over-MCP
 * before it was `leta`, and the model-facing surface did not move. A verb list that
 * mirrored a CLI's subcommands would have made that swap a prompt rewrite.
 *
 * LAZY, LITERALLY. Nothing here spawns at turn start, at bridge construction, or at
 * prompt assembly. `lspAvailable()` stats the filesystem (that is what gates the
 * prompt section) and the first `call()` is what registers the workspace and wakes
 * the daemon. A turn that never asks about a symbol never pays for a language
 * server.
 *
 * REPORTED ONCE, THEN LATCHED. Once a call fails at the BACKEND level the bridge
 * latches: every later verb in that turn rejects immediately, without spawning
 * anything, with a short message pointing at the failure that was already reported.
 * That is the mechanical half of "do not retry other verbs" — the prompt asks the
 * model not to, and the latch means it costs nothing when it does anyway. The latch
 * is per bridge, and a bridge is per turn, so the next turn tries again: the user may
 * have just installed the thing.
 *
 * NOTE (delta from `src/mcp/lsp.ts`, which this ports): that version treated EVERY
 * non-zero exit as a backend failure and returned raw stdout for a success. Both are
 * wrong for the two behaviours above. A grep-shaped backend exits non-zero when it
 * finds nothing, so "no matches" arrived as "the backend is broken"; and an empty
 * stdout arrived at the model as an empty string, which reads as a broken tool rather
 * than as the answer it is. `classify()` and `emptyAnswer()` are the fix. The port's
 * read-only-mirror refusal for `rename` is dropped with the mirror concept it guarded.
 *
 * `rename` is the one verb that WRITES. It edits the checkout directly, so it goes
 * around `patch`'s hash anchoring — the safeguard under shared-checkout delegation
 * (spec §7). It stays in the curated set because renaming by hand across call sites
 * is worse, but it is the reason concurrent agents should not be renaming into each
 * other's files.
 */
import { statSync } from "node:fs";
import { z } from "zod";
import { LspError } from "../errors.ts";
import { HOST_FN_VERBS } from "../harness/protocol.ts";

// ---------------------------------------------------------------------------
// The backend binary
// ---------------------------------------------------------------------------

/** The CLI behind the verbs. Swappable by design — see the module header. */
export const BACKEND_NAME = "leta";

/**
 * Where the binary may live beyond `$PATH`. A launchd-spawned server inherits a
 * minimal PATH with none of the Homebrew bins an interactive shell has, so a backend
 * the user can run in their terminal is invisible to the server without this.
 */
export const EXTRA_BIN_DIRS = ["/opt/homebrew/bin", "/usr/local/bin"];

/** An explicit absolute path to the backend, for installs the PATH scan misses. */
export const BIN_ENV_VAR = "BOUGH_LSP_BIN";

/** Injectable environment, so the lookup is testable without touching the real one. */
export interface BinLookup {
  /** Absent = reading `process.env`. */
  env?: (name: string) => string | undefined;
  /** Absent = `statSync`, answering "is this a file". */
  isFile?: (path: string) => boolean;
}

function envGet(deps: BinLookup): (name: string) => string | undefined {
  return deps.env ?? ((name) => process.env[name]);
}

function isFileAt(deps: BinLookup): (path: string) => boolean {
  return deps.isFile ?? ((path) => {
    try {
      return statSync(path).isFile();
    } catch {
      return false;
    }
  });
}

/**
 * The absolute path to the backend, or `undefined` when it is not installed.
 *
 * `BOUGH_LSP_BIN` wins outright: an explicit path is the user saying where it is,
 * and second-guessing it with a PATH scan would make a deliberate override silently
 * inert. Otherwise PATH first (the user's own ordering), then the extra dirs.
 *
 * This only stats. It never spawns — it is called at prompt assembly on every turn,
 * and a lookup that woke a daemon would break the laziness the whole design rests on.
 */
export function findBackend(deps: BinLookup = {}): string | undefined {
  const env = envGet(deps);
  const isFile = isFileAt(deps);
  const explicit = env(BIN_ENV_VAR);
  if (explicit) return isFile(explicit) ? explicit : undefined;
  const dirs = [...(env("PATH")?.split(":") ?? []), ...EXTRA_BIN_DIRS];
  for (const dir of dirs) {
    if (!dir) continue;
    const candidate = `${dir.replace(/\/+$/, "")}/${BACKEND_NAME}`;
    if (isFile(candidate)) return candidate;
  }
  return undefined;
}

/**
 * Is symbol navigation available at all? The prompt gate (spec §6: a host function
 * exists only when the prompt grants it) and the boot report both read this.
 *
 * Deliberately NOT memoized. It is one stat per PATH entry, and caching it would
 * mean a user who installs the backend mid-session keeps being told for the life of
 * the process that they have no symbol navigation.
 */
export function lspAvailable(deps: BinLookup = {}): boolean {
  return findBackend(deps) !== undefined;
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/**
 * The curated verb list, read from the canonical one in `harness/protocol.ts` so the
 * worker's `lsp.*` method object and this dispatcher cannot drift.
 */
export const LSP_VERBS: readonly string[] = HOST_FN_VERBS.lsp;

const Symbol_ = z.string().min(1);
const Context = z.number().int().nonnegative().optional();

/**
 * Argument shapes, validated at the boundary (plan §0). The program is arbitrary
 * model-written JavaScript, so a wrong shape is a thing that happens — and it must
 * come back as a sentence naming the parameter, never as a backend invocation with a
 * missing argv slot.
 */
const ARGS = {
  find: z.object({ pattern: z.string().min(1), path: z.string().min(1).optional() }),
  overview: z.object({ path: z.string().min(1) }),
  show: z.object({ symbol: Symbol_, context: Context }),
  def: z.object({ symbol: Symbol_ }),
  refs: z.object({ symbol: Symbol_, context: Context }),
  impls: z.object({ symbol: Symbol_ }),
  calls: z.object({ to: z.string().min(1).optional(), from: z.string().min(1).optional() }),
  rename: z.object({ symbol: Symbol_, new_name: z.string().min(1) }),
} satisfies Record<string, z.ZodTypeAny>;

type Verb = keyof typeof ARGS;

/**
 * The parameter a bare string means, per verb.
 *
 * `lsp.def("Gate.decide")` is what a model writes when it is moving fast, and
 * rejecting it teaches nothing — the intent is unambiguous for every verb that takes
 * exactly one required string. `calls` is absent on purpose: `lsp.calls("f")` cannot
 * say whether it means callers or callees, and guessing would answer a question
 * nobody asked.
 */
const BARE_STRING_KEY: Partial<Record<Verb, string>> = {
  find: "pattern",
  overview: "path",
  show: "symbol",
  def: "symbol",
  refs: "symbol",
  impls: "symbol",
  rename: "symbol",
};

/** verb → backend argv. The whole of the coupling to the backing CLI. */
const ARGV: Record<Verb, (a: Record<string, unknown>) => string[]> = {
  find: (a) => ["grep", a.pattern as string, ...(a.path ? [a.path as string] : [])],
  // "every symbol in this file" is a match-anything grep scoped to a path — the
  // backend has no separate outline subcommand, and the model should not have to know.
  overview: (a) => ["grep", ".", a.path as string],
  show: (a) => ["show", a.symbol as string, ...ctxFlag(a)],
  def: (a) => ["declaration", a.symbol as string],
  refs: (a) => ["refs", a.symbol as string, ...ctxFlag(a)],
  impls: (a) => ["implementations", a.symbol as string],
  calls: (a) => ["calls", ...(a.to ? ["--to", a.to as string] : ["--from", a.from as string])],
  rename: (a) => ["rename", a.symbol as string, a.new_name as string],
};

function ctxFlag(a: Record<string, unknown>): string[] {
  return typeof a.context === "number" ? ["--context", String(a.context)] : [];
}

function isVerb(verb: string): verb is Verb {
  return Object.prototype.hasOwnProperty.call(ARGS, verb);
}

/**
 * Validate one call and build its argv.
 *
 * Throws `LspError(400)` — a QUERY error. Nothing here has touched the backend, so
 * the message must not read like a backend failure: the fix is a corrected argument,
 * and the next verb will work fine.
 */
export function buildArgv(verb: string, args: unknown): string[] {
  if (!isVerb(verb)) {
    throw new LspError(
      400,
      `unknown lsp verb "${verb}" — the verbs are ${LSP_VERBS.join(", ")}.`,
    );
  }
  const raw = normalizeArgs(verb, args);
  const parsed = ARGS[verb].safeParse(raw);
  if (!parsed.success) {
    throw new LspError(400, `lsp.${verb}: ${issues(parsed.error)}. ${usage(verb)}`);
  }
  const value = parsed.data as Record<string, unknown>;
  if (verb === "calls" && !value.to === !value.from) {
    throw new LspError(
      400,
      `lsp.calls: pass exactly one of {to} (who calls this) or {from} (what this ` +
        `calls) — ${value.to ? "both were given" : "neither was given"}. ${usage("calls")}`,
    );
  }
  return ARGV[verb](value);
}

/** `null`/`undefined` → `{}`; a bare string → the verb's primary parameter. */
function normalizeArgs(verb: Verb, args: unknown): unknown {
  if (args === null || args === undefined) return {};
  if (typeof args === "string") {
    const key = BARE_STRING_KEY[verb];
    return key ? { [key]: args } : args;
  }
  return args;
}

function issues(error: z.ZodError): string {
  return error.issues
    .map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`)
    .join("; ");
}

/** One-line call shapes, so a rejected call carries its own fix. */
function usage(verb: Verb): string {
  const shapes: Record<Verb, string> = {
    find: 'lsp.find({pattern: "Gate", path?: "src/"})',
    overview: 'lsp.overview({path: "src/gate.ts"})',
    show: 'lsp.show({symbol: "Gate.decide", context?: 3})',
    def: 'lsp.def({symbol: "Gate.decide"})',
    refs: 'lsp.refs({symbol: "Gate.decide", context?: 2})',
    impls: 'lsp.impls({symbol: "Gate"})',
    calls: 'lsp.calls({to: "Gate.decide"}) or lsp.calls({from: "Gate.decide"})',
    rename: 'lsp.rename({symbol: "Gate.decide", new_name: "Gate.choose"})',
  };
  return `Call it as ${shapes[verb]}.`;
}

// ---------------------------------------------------------------------------
// Running the backend
// ---------------------------------------------------------------------------

/** One backend invocation's raw result. */
export interface LspExec {
  code: number;
  stdout: string;
  stderr: string;
}

/** How a backend invocation is performed. Injected, so tests need no binary. */
export type LspRun = (
  args: string[],
  opts: { cwd: string; signal?: AbortSignal },
) => Promise<LspExec>;

/** Wall clock ceiling for one invocation. The FIRST call indexes, so it is generous. */
export const CALL_TIMEOUT_MS = 120_000;

/**
 * The production runner: the backend as a plain subprocess.
 *
 * The env is rebuilt rather than inherited wholesale for the same reason
 * `EXTRA_BIN_DIRS` exists — the daemon spawns language servers (node, tsserver,
 * gopls) as children, and under launchd the bare PATH strands every one of them.
 *
 * The turn's interrupt is wired straight into the child (spec §5: a program's
 * children are killed when the turn is interrupted). This subprocess is spawned
 * HOST-side, so the program worker's own child sweep never sees it — without this
 * signal an interrupted turn would leave the backend running.
 */
export function spawnRunner(bin: string): LspRun {
  return async (args, opts) => {
    const path = [process.env["PATH"], ...EXTRA_BIN_DIRS].filter(Boolean).join(":");
    // Bun.spawn's `env` REPLACES the environment, so the inherited one is spread in
    // first: only PATH is meant to change, and the backends need the rest (HOME, etc.).
    const proc = Bun.spawn([bin, ...args], {
      cwd: opts.cwd,
      env: { ...process.env, PATH: path },
      stdout: "pipe",
      stderr: "pipe",
      ...(opts.signal ? { signal: opts.signal } : {}),
    });
    const [stdout, stderr, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    return { code, stdout, stderr };
  };
}

// ---------------------------------------------------------------------------
// Classification — the heart of this module
// ---------------------------------------------------------------------------

/** What one invocation turned out to mean. */
export type LspOutcome =
  /** The backend answered with something. An ordinary success. */
  | { kind: "text"; text: string }
  /** The backend answered with nothing. Also an ordinary success (spec §10). */
  | { kind: "empty" }
  /** The call was wrong — a bad path, an ambiguous symbol. The backend is fine. */
  | { kind: "query"; detail: string }
  /** The backend itself failed. Everything downstream drops to rg for the task. */
  | { kind: "backend"; detail: string };

/**
 * Phrases that can only mean the BACKEND is broken. Checked first, and kept narrow
 * on purpose: every pattern here costs the model symbol navigation for the rest of
 * the task, so a phrase that could plausibly describe a bad query does not belong.
 *
 * Note what is deliberately absent: "no such file or directory". A missing path is
 * far more often `lsp.overview({path: "src/typo.ts"})` than a broken install, and
 * classifying it as a backend failure would retire the verbs over a typo.
 */
const BACKEND_RE =
  /command not found|: not found|permission denied|connection refused|daemon|language server|failed to start|could not start|unable to start|panic|segmentation fault|out of memory|timed out|timeout/i;

/**
 * Phrases meaning "the backend looked and there was nothing there". A grep-shaped
 * CLI exits non-zero for this, which is exactly the case the port got wrong.
 */
const EMPTY_RE =
  /\bno (?:results?|matches?|match|symbols?|references?|implementations?|callers?|callees?|definitions?|declarations?)\b|nothing found|not found/i;

/** Phrases meaning the CALL was wrong. Recoverable by changing the arguments. */
const QUERY_RE =
  /ambiguous|did you mean|candidates|usage:|invalid|unrecognized|unknown (?:flag|option|argument)|no such file or directory|not a directory|is a directory|unsupported (?:file|language|extension)/i;

/**
 * Decide what an invocation meant. Pure, and the only place the decision is made.
 *
 * The ladder, in order, and why:
 *   1. **Exit 0** is an answer — empty stdout means an empty answer, nothing else.
 *   2. **Backend phrases** win over everything, so a language server that failed to
 *      start is never read as "no results".
 *   3. **Empty phrases** next, including a bare "not found": "symbol not found" IS
 *      the answer to "where is this symbol", and the single most common non-zero
 *      exit a navigation CLI produces.
 *   4. **Query phrases** — ambiguity, a bad path, a bad flag. The backend answered;
 *      the question was wrong.
 *   5. **Exit code shapes**: 126/127 and signal deaths (>128) are the shell's way of
 *      saying the binary did not run. Exit 1 with nothing on stderr is the grep
 *      convention for "no matches".
 *   6. **Anything left** is a backend failure. An unexplained non-zero exit with
 *      prose we cannot read is more likely a broken tool than a bad question, and
 *      the consequence of that call is bounded: the model drops to rg and finishes
 *      the job, which is a slower correct answer rather than a wrong one.
 */
export function classify(res: LspExec): LspOutcome {
  const out = res.stdout.trim();
  const err = res.stderr.trim();
  if (res.code === 0) return out === "" ? { kind: "empty" } : { kind: "text", text: out };

  const said = err || out;
  if (BACKEND_RE.test(said)) return { kind: "backend", detail: oneLine(said) };
  if (EMPTY_RE.test(said)) return { kind: "empty" };
  if (QUERY_RE.test(said)) return { kind: "query", detail: oneLine(said) };
  if (res.code === 126 || res.code === 127 || res.code > 128 || res.code < 0) {
    return { kind: "backend", detail: oneLine(said) || `exit ${res.code}` };
  }
  if (res.code === 1 && err === "") return { kind: "empty" };
  return { kind: "backend", detail: oneLine(said) || `exit ${res.code}` };
}

/** Backend prose folded to one line and capped — it lands in a transcript card. */
function oneLine(text: string, max = 400): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

// ---------------------------------------------------------------------------
// Message text — a product surface (spec §6)
// ---------------------------------------------------------------------------

/**
 * What an empty result SAYS. This is a resolved value, not a rejection, and the
 * words are the whole point: the model has to be able to tell "I looked and there is
 * nothing" from "the tool is broken" out of one string, because that string is all
 * it gets.
 */
export function emptyAnswer(verb: string, args: unknown): string {
  return `lsp.${verb}${subject(args)}: no results.\n` +
    `This is an ordinary answer, not a failure — the backend answered and found ` +
    `nothing. Usually the name is spelled differently or the symbol lives somewhere ` +
    `else. Adjust the query, or use rg for THIS lookup, and keep using lsp for the ` +
    `next one.`;
}

/** The backend failed, first time this turn: the full diagnosis and the whole plan. */
export function backendDownMessage(verb: string, detail: string): string {
  return `lsp.${verb} could not run: the ${BACKEND_NAME} BACKEND failed — ${detail}. ` +
    `This is the backend, not a missing symbol: nothing is known about the symbol ` +
    `either way, and no lsp.* verb will work for the rest of this task. Drop to rg + ` +
    `view + patch now, mention the backend in one line, and finish the job. Do not ` +
    `retry other lsp verbs to confirm it, and do not treat it as blocking.`;
}

/** Every later verb once the latch is set. Short — it has already been explained. */
export function backendLatchedMessage(verb: string, detail: string): string {
  return `lsp.${verb}: the ${BACKEND_NAME} backend already failed this turn (${detail}) ` +
    `and was not called again. Use rg + view + patch for the rest of the task.`;
}

/** The subject of a call, for the empty answer's first line. Best-effort. */
function subject(args: unknown): string {
  if (typeof args === "string") return `("${args}")`;
  if (args && typeof args === "object") {
    const a = args as Record<string, unknown>;
    const value = a.symbol ?? a.pattern ?? a.path ?? a.to ?? a.from;
    if (typeof value === "string") return `("${value}")`;
  }
  return "";
}

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

export interface LspBridgeOptions {
  /** The checkout every invocation runs in — the session's workspace. */
  workspace: string;
  /** How to invoke the backend. Absent = spawn the located binary. */
  run?: LspRun;
  /** Binary lookup seams, when `run` is absent. */
  bin?: BinLookup;
  /** The turn's interrupt, wired into every child. */
  signal?: AbortSignal;
  /** Per-invocation ceiling. Absent = `CALL_TIMEOUT_MS`. */
  timeoutMs?: number;
  /**
   * Called at most ONCE per bridge, when the backend is first found to be broken.
   * The host logs it; the latch is what keeps it to once (plan T7.3: reported once,
   * and it does not retry every verb).
   */
  onBackendDown?: (detail: string) => void;
}

export interface LspBridge {
  /** Run one verb. Resolves with plain text; rejects with `LspError`. */
  call(verb: string, args: unknown): Promise<string>;
  /** The latched failure, once there is one. Exposed so a host can report state. */
  readonly down: string | undefined;
}

/**
 * Build the per-turn bridge behind `lsp.*`.
 *
 * Construction spawns NOTHING. The first call registers the workspace with the
 * daemon (idempotent daemon-side) and that registration is memoized for the life of
 * the bridge — later calls, and later turns, find the language server already warm.
 *
 * NOTE (delta from the port): a failed registration is not retried within the turn.
 * The port cleared its memo so the next verb could try again, which is precisely the
 * "retry every verb against a dead backend" the spec forbids. The latch replaces it,
 * and the retry happens where it can plausibly succeed: the next turn, on a fresh
 * bridge.
 */
export function createLspBridge(opts: LspBridgeOptions): LspBridge {
  const timeoutMs = opts.timeoutMs ?? CALL_TIMEOUT_MS;
  let down: string | undefined;
  let registered: Promise<void> | undefined;

  /** Resolve the runner lazily: locating the binary is a stat, not a spawn. */
  const runner = (): LspRun => {
    if (opts.run) return opts.run;
    const bin = findBackend(opts.bin ?? {});
    if (!bin) {
      throw new LspError(
        502,
        `${BACKEND_NAME} is not installed (looked on PATH, in ` +
          `${EXTRA_BIN_DIRS.join(", ")}, and at $${BIN_ENV_VAR})`,
      );
    }
    return spawnRunner(bin);
  };

  /**
   * One invocation, with the turn's interrupt and the deadline folded into one
   * signal. An interrupt is NOT a backend failure — it must not latch, or a stopped
   * turn would poison a capability the next one is entitled to.
   */
  const exec = async (argv: string[]): Promise<LspExec> => {
    const run = runner();
    const controller = new AbortController();
    const timer = setTimeout(
      () => controller.abort(new Error(`no answer in ${timeoutMs}ms`)),
      timeoutMs,
    );
    const onInterrupt = () => controller.abort(opts.signal?.reason);
    opts.signal?.addEventListener("abort", onInterrupt, { once: true });
    try {
      return await run(argv, { cwd: opts.workspace, signal: controller.signal });
    } finally {
      clearTimeout(timer);
      opts.signal?.removeEventListener("abort", onInterrupt);
    }
  };

  const fail = (verb: string, detail: string): never => {
    if (down === undefined) {
      down = detail;
      opts.onBackendDown?.(detail);
      throw new LspError(502, backendDownMessage(verb, detail));
    }
    throw new LspError(502, backendLatchedMessage(verb, down));
  };

  const register = async (): Promise<void> => {
    const res = await exec(["workspace", "add"]);
    if (res.code !== 0) {
      const said = oneLine(res.stderr.trim() || res.stdout.trim()) || `exit ${res.code}`;
      throw new LspError(502, said);
    }
  };

  return {
    get down() {
      return down;
    },

    call: async (verb: string, args: unknown): Promise<string> => {
      // A latched backend answers before anything is validated or spawned: the
      // point of the latch is that the second verb costs nothing.
      if (down !== undefined) throw new LspError(502, backendLatchedMessage(verb, down));
      // Stop before the side effect, not after it — the same rule the turn runner
      // applies to tool calls. An already-interrupted turn spawns nothing, and this
      // is emphatically not a backend failure, so it must not latch.
      if (opts.signal?.aborted) throw interrupted(verb);

      // Validation first, and it throws a 400 that never latches — a wrong argument
      // says nothing at all about whether the backend works.
      const argv = buildArgv(verb, args);

      try {
        registered ??= register();
        await registered;
      } catch (err) {
        if (opts.signal?.aborted) throw interrupted(verb);
        return fail(verb, detailOf(err));
      }

      let res: LspExec;
      try {
        res = await exec(argv);
      } catch (err) {
        // A thrown invocation is a backend failure — the binary vanished, the
        // deadline passed, the spawn was refused — EXCEPT when the turn was
        // interrupted, which is the user, not the tool.
        if (opts.signal?.aborted) throw interrupted(verb);
        return fail(verb, detailOf(err));
      }

      const outcome = classify(res);
      switch (outcome.kind) {
        case "text":
          return outcome.text;
        case "empty":
          return emptyAnswer(verb, args);
        case "query":
          throw new LspError(
            400,
            `lsp.${verb}: ${outcome.detail}. The backend is working — this is about ` +
              `the query, so refine it (a symbol is a name or a dot path like ` +
              `"Gate.decide") and keep using lsp.`,
          );
        case "backend":
          return fail(verb, outcome.detail);
      }
    },
  };
}

/** The turn was interrupted mid-call. Catchable, and it does not latch. */
function interrupted(verb: string): LspError {
  return new LspError(
    400,
    `lsp.${verb}: the turn was interrupted before the backend answered. Nothing was ` +
      `learned about the symbol; the backend itself is fine.`,
  );
}

function detailOf(err: unknown): string {
  if (err instanceof LspError) return oneLine(err.message);
  return oneLine(err instanceof Error ? err.message : String(err));
}
