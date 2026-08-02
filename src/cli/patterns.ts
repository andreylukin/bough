/**
 * `bough patterns [FILE]` — compress a log into the handful of statements it is
 * actually made of.
 *
 * WHY THIS EXISTS. Reading a log is the single most common thing an agent does that
 * it is worst at. A 200,000-line file cannot go into a context window, `tail` shows
 * the end rather than the problem, and `grep ERROR` finds the lines that say ERROR
 * rather than the ones that matter. What the reader needs is the structure — there
 * are forty distinct statements here, three of them are failures, one of those
 * started at 14:22 and is about a single host — and that is a summary no amount of
 * clever grepping produces, because it requires counting.
 *
 * WHY IT IS `patterns` AND NOT `logs`. `bough logs` already exists and tails the
 * server's own log. Two subcommands one letter apart, one of which reads bough's
 * log and the other of which reads yours, is a trap that would be sprung mostly by
 * people debugging something else at the time.
 *
 * WHY A SUBCOMMAND AND NOT A HOST FUNCTION. The pipeline could have been bound into
 * the program scope as `patterns()`, and it deliberately is not. `lsp.*` was removed
 * from that scope for want of use and replaced by a skill teaching a CLI, and the
 * lesson generalizes: a host function is a permanent widening of every program's
 * API and of the system prompt that must describe it, whereas a subcommand costs
 * nothing until something runs it, is reachable from a shell and a script as well as
 * from a turn, and can be deleted without a migration. If this earns a place in the
 * program scope later, that is a decision made with usage data instead of ahead of
 * it.
 *
 * Conventions are `cli/mcp.ts`'s, for the reasons stated there:
 *
 *   - **Argument parsing is pure and total.** `parseArgs` is a function over a
 *     string array returning arguments or a usage error. It never reads the
 *     environment, never exits, never throws.
 *   - **Every effect is injected.** `runPatterns` takes its input source and two
 *     writers and RETURNS an exit code. The `import.meta.main` block is the only
 *     code that touches a real process, which is what lets the whole command be
 *     tested without a file or a pipe.
 *
 * Exit codes:
 *
 *   0  the log was analyzed
 *   1  the input could not be read
 *   2  usage problem
 *
 * There is deliberately no "found errors" exit code. The command reports what is in
 * a file; whether an ERROR line is a failure is a question about the caller's
 * intent, and a non-zero exit would make `bough patterns` unusable in the pipelines
 * where it is most useful.
 */
import { Analyzer } from "../logs/analyze.ts";
import { toHuman, toJson, toLlm } from "../logs/format.ts";

export type Format = "llm" | "json" | "human";

export interface PatternArgs {
  /** The file to read, or `undefined` for stdin. */
  file?: string;
  format?: Format;
  top: number;
  colour?: boolean;
  /** Similarity threshold override for the clustering pass. */
  threshold?: number;
  refYear?: number;
}

export interface UsageError {
  usageError: string;
}

export const USAGE = [
  "usage: bough patterns [OPTIONS] [FILE]",
  "",
  "  Compress a log into its distinct statements, with per-variable statistics,",
  "  anomalies and correlations. Reads stdin when FILE is absent or is `-`.",
  "",
  "  --llm             compact markdown for a model to read",
  "  --json            structured output (the shape is stable)",
  "  --human           colored terminal output",
  "                    default: --human on a terminal, --llm otherwise",
  "",
  "  --top N           patterns to show (default 20)",
  "  --threshold F     clustering similarity, 0..1 (default 0.4). Raise it if",
  "                    distinct statements are being merged; lower it if one",
  "                    statement is splitting into near-duplicate patterns",
  "  --year Y          year for timestamp formats that omit one, e.g. syslog",
  "  --no-color        never emit ANSI, even on a terminal",
  "  -h, --help        this",
  "",
  "exit: 0 analyzed · 1 unreadable input · 2 usage",
].join("\n");

/** Everything the command needs from the world. */
export interface PatternDeps {
  /** Yields the input's lines. Rejects if the source cannot be read. */
  readLines: (file: string | undefined) => AsyncIterable<string> | Iterable<string>;
  out: (line: string) => void;
  err: (line: string) => void;
  /** Whether stdout is a terminal, which picks the default format and colour. */
  isTty: boolean;
  /** Terminal width for the human view. */
  width?: number;
}

export function isUsageError(v: PatternArgs | UsageError): v is UsageError {
  return "usageError" in v;
}

/**
 * Parse argv. Pure and total: every bad input becomes a `usageError` string.
 *
 * Flags may appear before or after the file, because people type them in both
 * orders and refusing one of them is a papercut with no upside.
 */
export function parseArgs(argv: readonly string[]): PatternArgs | UsageError {
  const args: PatternArgs = { top: 20 };
  let sawFile = false;

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i] as string;
    switch (a) {
      case "-h":
      case "--help":
        return { usageError: "" };
      case "--llm":
      case "--json":
      case "--human": {
        const f = a.slice(2) as Format;
        // An explicit second format is a contradiction, not a refinement. Silently
        // taking the last one produces output the caller did not ask for and will
        // parse with the wrong reader.
        if (args.format && args.format !== f) {
          return { usageError: `--${args.format} and ${a} cannot both be given` };
        }
        args.format = f;
        break;
      }
      case "--no-color":
      case "--no-colour":
        args.colour = false;
        break;
      case "--top": {
        const v = Number(argv[++i]);
        if (!Number.isInteger(v) || v < 1) return { usageError: "--top needs a positive integer" };
        args.top = v;
        break;
      }
      case "--threshold": {
        const v = Number(argv[++i]);
        if (!(v > 0 && v <= 1)) return { usageError: "--threshold needs a number in (0,1]" };
        args.threshold = v;
        break;
      }
      case "--year": {
        const v = Number(argv[++i]);
        if (!Number.isInteger(v) || v < 1970 || v > 9999) {
          return { usageError: "--year needs a four-digit year" };
        }
        args.refYear = v;
        break;
      }
      case "-":
        // The conventional spelling of "stdin, explicitly". Leaves `file` unset.
        sawFile = true;
        break;
      default:
        if (a.startsWith("-")) return { usageError: `unknown option ${a}` };
        if (sawFile) return { usageError: "only one FILE may be given" };
        sawFile = true;
        args.file = a;
    }
  }
  return args;
}

/** Run the command. Returns an exit code; never exits the process itself. */
export async function runPatterns(
  argv: readonly string[],
  deps: PatternDeps,
): Promise<number> {
  const parsed = parseArgs(argv);
  if (isUsageError(parsed)) {
    if (parsed.usageError === "") {
      deps.out(USAGE);
      return 0;
    }
    deps.err(`error: ${parsed.usageError}`);
    deps.err(USAGE);
    return 2;
  }

  // The default format follows the consumer, not a preference. On a terminal a
  // person is reading; off one, something else is, and that something is far more
  // often a model or a script than a person running `less`.
  const format: Format = parsed.format ?? (deps.isTty ? "human" : "llm");
  const colour = parsed.colour ?? deps.isTty;

  // Lines are pushed through as they arrive and never collected. Buffering them
  // first is the obvious shape and it silently caps the tool at whatever fits in
  // memory — a 48MB log costs ~700MB as an array of strings, which would make every
  // bounded sketch behind this pointless.
  const analyzer = new Analyzer({
    top: parsed.top,
    ...(parsed.refYear !== undefined ? { refYear: parsed.refYear } : {}),
    ...(parsed.threshold !== undefined ? { drain: { threshold: parsed.threshold } } : {}),
  });
  try {
    const src = deps.readLines(parsed.file);
    if (Symbol.asyncIterator in src) {
      for await (const line of src as AsyncIterable<string>) analyzer.push(line);
    } else {
      for (const line of src as Iterable<string>) analyzer.push(line);
    }
  } catch (e) {
    deps.err(`error: cannot read ${parsed.file ?? "stdin"}: ${(e as Error).message}`);
    return 1;
  }
  const analysis = analyzer.finish();

  // An empty input is not an error — an empty log file is a perfectly ordinary
  // thing to point this at, and a non-zero exit would break the pipelines that do.
  if (analysis.lines === 0) {
    deps.err("no log lines found");
    if (format === "json") deps.out(toJson(analysis));
    return 0;
  }

  const rendered =
    format === "json"
      ? toJson(analysis)
      : format === "llm"
        ? toLlm(analysis)
        : toHuman(analysis, colour, deps.width ?? 80);
  deps.out(rendered.replace(/\n$/, ""));
  return 0;
}

if (import.meta.main) {
  const code = await runPatterns(process.argv.slice(2), {
    readLines: (file) => realLines(file),
    out: (l) => console.log(l),
    err: (l) => console.error(l),
    isTty: Boolean(process.stdout.isTTY),
    ...(process.stdout.columns ? { width: process.stdout.columns } : {}),
  });
  process.exit(code);
}

/**
 * Read a file or stdin as lines, without holding the whole input as one string.
 *
 * A file is streamed rather than `readFileSync`'d because the whole point of the
 * tool is inputs too big to hold comfortably, and reading one 2GB string to then
 * split it is the one move that would make the memory-bounded pipeline behind it
 * pointless.
 */
async function* realLines(file: string | undefined): AsyncGenerator<string> {
  const stream =
    file === undefined ? Bun.stdin.stream() : Bun.file(file).stream();
  const decoder = new TextDecoder();
  let carry = "";
  for await (const chunk of stream as AsyncIterable<Uint8Array>) {
    // `stream: true` is what makes a multi-byte character split across a chunk
    // boundary survive; without it the decoder emits a replacement character and
    // the line silently differs from the file.
    carry += decoder.decode(chunk, { stream: true });
    const parts = carry.split("\n");
    carry = parts.pop() ?? "";
    for (const p of parts) yield p;
  }
  carry += decoder.decode();
  if (carry.length > 0) yield carry;
}
