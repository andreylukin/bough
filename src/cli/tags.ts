/**
 * `bough tags` — what the command memory knows, and what it tells the model.
 *
 * WHY THIS EXISTS. The tag memory has been shaping every turn from behind a curtain:
 * a priming note the user never sees, ranked by arithmetic they cannot inspect, over
 * a table they can only reach through `sqlite3` or by asking the agent to query
 * itself. Three things follow from that, and this command answers all three.
 *
 *   - **What is the model being told about this project?** The default view IS the
 *     priming note's ranking, with the numbers it sorted by, so a surprising tag is
 *     traceable to the commands behind it rather than taken on faith.
 *   - **What worked under this tag?** `show` is the human's `history.sql()` — the
 *     same recall the program gets, without spending a turn to ask for it.
 *   - **Is any of this working?** `stats` is the measurement the whole tag arc has
 *     been missing: vocabulary size and tag coverage per day, so a prompt change or
 *     a note change reads as a step on a date instead of as a feeling.
 *
 * WHY A SUBCOMMAND AND NOT A TAB. Same reasoning `patterns` states: a panel is a
 * permanent surface with a keymap and a rendering budget, a subcommand costs nothing
 * until it is run, and this is a thing you reach for occasionally and read carefully
 * rather than watch. It also works with the server stopped, which matters when the
 * question is "what did I run before it broke".
 *
 * NO SERVER, AND ONLY READS. The database is opened directly — every query here is a
 * SELECT, and they live in `db/db.ts` beside the ones the prompt uses so the ranking
 * this prints cannot drift from the ranking the model gets.
 *
 * Conventions are `cli/mcp.ts`'s: parsing is pure and total, effects are injected,
 * `runTags` returns an exit code and never touches a real process.
 *
 * Exit codes:
 *
 *   0  answered
 *   1  there is no command memory yet
 *   2  usage problem
 */
import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";
import { enableSqliteExtensions } from "../db/extensions.ts";
import { createEmbedLayer } from "../history/embed.ts";
import type { Db, TagDiversityDay, TaggedCommand } from "../types.ts";
import { openDb } from "../db/db.ts";
import { dbPath } from "../paths.ts";
import { type RankedTag, rankedRepoTags, workspaceRepo } from "../history/stats.ts";

export type TagsVerb = "list" | "show" | "stats" | "sql" | "similar";

/** Bounded so one greedy SELECT cannot flood a terminal — or a tool result. */
const MAX_ROWS = 200;

/** The tables a query may read. Names only — the message a refusal shows. */
const SURFACE =
  "command_history, command_tags, command_dirs, command_history_fts, messages, messages_fts, sessions, turns";

export interface TagsArgs {
  verb: TagsVerb;
  /** `show` only: the tag to open. */
  tag?: string;
  /** A repo identity (git origin URL, or a path). Absent = this checkout's. */
  repo?: string;
  /** `--all`: no repo scope at all, so the memory answers across projects. */
  allRepos: boolean;
  limit: number;
  days: number;
  json: boolean;
  /** `show`: print the whole program each command ran in, not just its size. */
  program: boolean;
}

export interface UsageError {
  usageError: string;
}

export const USAGE = [
  "usage: bough tags [VERB] [OPTIONS]",
  "",
  "  (none)          this project's tag vocabulary — what the model is primed with",
  "  show TAG        the commands recorded under TAG, newest first",
  "  stats           tag coverage and vocabulary per day — did anything change?",
  "  sql QUERY       a read-only SELECT over the memory and the transcripts",
  "  similar TEXT    semantic recall, where the local vector layer exists",
  "",
  "  --repo R        scope to a repo identity (origin URL or path); default: here",
  "  --all           every repo the memory knows, not just this one",
  "  --program       show: print the program each command ran in, not just its size",
  "  --limit N       rows (default 20)",
  "  --days N        stats: how far back to look (default 30)",
  "  --json          machine-readable output",
  "  -h, --help      this",
  "",
  "exit: 0 answered · 1 no command memory yet · 2 usage",
].join("\n");

const NUMERIC = new Set(["--limit", "--days"]);

/** Pure and total: arguments in, arguments or a usage error out. Never throws. */
export function parseTagsArgs(argv: readonly string[]): TagsArgs | UsageError {
  const args: TagsArgs = {
    verb: "list",
    limit: 20,
    days: 30,
    json: false,
    allRepos: false,
    program: false,
  };
  const positional: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-h" || a === "--help") return { usageError: USAGE };
    if (a === "--json") {
      args.json = true;
      continue;
    }
    if (a === "--all") {
      args.allRepos = true;
      continue;
    }
    if (a === "--program") {
      args.program = true;
      continue;
    }
    if (a === "--repo") {
      const v = argv[++i];
      if (v === undefined) return { usageError: `--repo needs a value\n${USAGE}` };
      args.repo = v;
      continue;
    }
    if (NUMERIC.has(a)) {
      const v = argv[++i];
      const n = Number(v);
      if (v === undefined || !Number.isFinite(n) || n <= 0) {
        return { usageError: `${a} needs a positive number\n${USAGE}` };
      }
      if (a === "--limit") args.limit = Math.trunc(n);
      else args.days = Math.trunc(n);
      continue;
    }
    if (a.startsWith("-")) return { usageError: `unknown option ${a}\n${USAGE}` };
    positional.push(a);
  }

  const [first, ...rest] = positional;
  if (first === "sql" || first === "similar") {
    if (rest.length !== 1) {
      return { usageError: `${first} needs exactly one quoted argument\n${USAGE}` };
    }
    args.verb = first;
    args.tag = rest[0];
  } else if (first === "show") {
    if (rest.length !== 1) return { usageError: `show needs exactly one TAG\n${USAGE}` };
    args.verb = "show";
    args.tag = rest[0];
  } else if (first === "stats") {
    if (rest.length > 0) return { usageError: `stats takes no arguments\n${USAGE}` };
    args.verb = "stats";
  } else if (first !== undefined) {
    // A bare word is the commonest thing to type and the likeliest to be a tag.
    // Guessing `show` for it beats a usage error that names three verbs.
    if (rest.length > 0) return { usageError: `unknown verb ${first}\n${USAGE}` };
    args.verb = "show";
    args.tag = first;
  }
  // `--all` is the absence of a scope, and it must beat an explicit `--repo`:
  // asking for everything after naming one is a correction, not a contradiction.
  if (args.allRepos) delete args.repo;
  return args;
}

export interface TagsDeps {
  db?: Db;
  /** The file `sql` opens read-only. Absent = the live `paths.dbPath()`. */
  dbFile?: string;
  /** The vector layer factory, injected so a test needs no extensions. */
  embed?: () => { similar(text: string): Promise<unknown[]>; close(): void } | null;
  /** Where "this checkout" is resolved from. Absent = the process's cwd. */
  cwd?: string;
  now?: () => number;
  out: (line: string) => void;
  err: (line: string) => void;
}

/**
 * A read-only SELECT over the whole database — what `history.sql()` used to be, and
 * now the only door to it.
 *
 * READ-ONLY IS ENFORCED TWICE, both at the connection: the handle is opened
 * `{readonly: true}` AND `PRAGMA query_only = ON`, which also covers anything a
 * clever statement ATTACHes. The keyword check on top exists only to answer a write
 * attempt with a sentence instead of a bare SQLITE_READONLY. That is the whole
 * reason this is a command rather than advice to run `sqlite3`: the guarantee is
 * structural, against a file a live server is writing to, instead of a convention
 * the caller is asked to remember.
 */
function querySql(path: string, sql: string): { rows: unknown[] } | { error: string } {
  const head = sql.replace(/^\s*(--[^\n]*\n|\/\*[\s\S]*?\*\/|\s)+/g, "").slice(0, 8)
    .toUpperCase();
  if (!head.startsWith("SELECT") && !head.startsWith("WITH")) {
    return {
      error: `read-only: a query must start with SELECT or WITH. Queryable: ${SURFACE}.`,
    };
  }
  let db: Database | undefined;
  try {
    db = new Database(path, { readonly: true });
    db.exec("PRAGMA query_only = ON");
    // A concurrent writer holding the journal must surface as a brief wait, not as
    // a spurious "database is locked".
    db.exec("PRAGMA busy_timeout = 2000");
    return { rows: (db.prepare(sql).all() as unknown[]).slice(0, MAX_ROWS) };
  } catch (err) {
    // The caller wrote the SQL; the driver's own message is what lets them fix it.
    return {
      error: `${err instanceof Error ? err.message : String(err)}. Queryable: ${SURFACE}.`,
    };
  } finally {
    db?.close();
  }
}

/** `2 days ago`, `3h ago` — a timestamp a reader can place without arithmetic. */
function ago(ts: number, now: number): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 90) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

/** Right-pad to `w` display columns. Tags and numbers here are ASCII. */
const pad = (s: string, w: number) => s.length >= w ? s : s + " ".repeat(w - s.length);
const lpad = (s: string, w: number) => s.length >= w ? s : " ".repeat(w - s.length) + s;

function renderList(ranked: RankedTag[], repo: string, out: (l: string) => void): void {
  out(`${repo}`);
  if (ranked.length === 0) {
    out("  no tagged commands yet — the model tags them as it runs them");
    return;
  }
  out("");
  out(`  ${pad("tag", 24)}${lpad("weight", 8)}${lpad("repos", 7)}${lpad("score", 8)}`);
  for (const r of ranked) {
    out(
      `  ${pad(r.tag, 24)}${lpad(r.weight.toFixed(1), 8)}${lpad(String(r.repos), 7)}${
        lpad(r.score.toFixed(1), 8)
      }`,
    );
  }
  out("");
  // The ordering rule, said once, because a table sorted by a column it does not
  // show is the kind of thing that reads as a bug.
  out("  ranked by weight × how FEW repos use the tag: a word every project uses");
  out("  names a tool, and this list is for the words that name this project.");
}

function renderShow(
  rows: TaggedCommand[],
  tag: string,
  now: number,
  out: (l: string) => void,
  program_: (messageId: string) => string | null,
  showProgram: boolean,
): void {
  if (rows.length === 0) {
    out(`no commands tagged "${tag}"`);
    return;
  }
  out(`${rows.length} command${rows.length === 1 ? "" : "s"} tagged "${tag}"`);
  out("");
  let lastTags = "";
  for (const r of rows) {
    // The full tag string only when it CHANGES. Every row under one tag usually
    // carries the same one, and repeating it doubles the output to say nothing —
    // where it differs is the interesting part (`bun:test:retention` among a run of
    // `bun:test:composer` is the row you were looking for).
    if (r.tags !== lastTags) {
      out(`  ${r.tags}`);
      lastTags = r.tags;
    }
    // The exit code first, because "what worked here" is the question this answers.
    const mark = r.exitCode === 0 ? "✓" : r.exitCode === null ? "·" : "✗";
    out(`    ${mark} ${pad(ago(r.ts, now), 9)} ${r.cmd.replace(/\s+/g, " ").slice(0, 96)}`);
    // …and the round it ran in, because on anything but a one-liner the program is
    // the reusable part and the command is a line inside it.
    const program = r.messageId === null ? null : program_(r.messageId);
    if (program === null) continue;
    if (showProgram) {
      for (const line of program.split("\n")) out(`      │ ${line}`);
    } else {
      const lines = program.split("\n").length;
      out(`      ↳ program: ${lines} line${lines === 1 ? "" : "s"} · --program to see it`);
    }
  }
}

function renderStats(days: TagDiversityDay[], out: (l: string) => void): void {
  if (days.length === 0) {
    out("no commands in that window");
    return;
  }
  out(
    `  ${pad("day", 12)}${lpad("sessions", 9)}${lpad("cmds", 6)}${lpad("tagged", 8)}${
      lpad("vocab", 7)
    }${lpad("refs", 6)}${lpad("uses", 6)}`,
  );
  for (const d of days) {
    // `tagged` as a share, because the absolute count says nothing without the
    // total — and the share is the number that moves when a leg goes untagged.
    const share = d.commands === 0 ? "—" : `${Math.round((d.tagged / d.commands) * 100)}%`;
    out(
      `  ${pad(d.day, 12)}${lpad(String(d.sessions), 9)}${lpad(String(d.commands), 6)}${
        lpad(share, 8)
      }${lpad(String(d.distinctTags), 7)}${lpad(String(d.distinctRefs), 6)}${
        lpad(String(d.tagUses), 6)
      }`,
    );
  }
  out("");
  out("  vocab is DISTINCT coined tags that day; refs are `linear.*`-style pointers,");
  out("  counted apart so a busy ticket week does not read as a richer vocabulary;");
  out("  uses is how often any tag was applied. vocab rising with uses flat is the");
  out("  model naming more things, which is the point; uses rising with vocab flat is");
  out("  it repeating itself.");
}

/**
 * Run the command. Returns an exit code; every effect is injected, so the whole
 * thing is testable against an in-memory database and two collectors.
 *
 * Async only because `similar` is — the vector layer embeds the query inside SQLite
 * and that is a real await. Every other verb resolves without yielding.
 */
export async function runTags(argv: readonly string[], deps: TagsDeps): Promise<number> {
  const parsed = parseTagsArgs(argv);
  if ("usageError" in parsed) {
    deps.err(parsed.usageError);
    return parsed.usageError === USAGE ? 0 : 2;
  }
  const now = (deps.now ?? Date.now)();

  let db = deps.db;
  if (!db) {
    if (!existsSync(dbPath())) {
      deps.err(`no command memory yet at ${dbPath()} — run something through bough first`);
      return 1;
    }
    db = openDb();
  }

  // `--all` is the one way to see across projects; otherwise the scope is this
  // checkout's identity, which is what the memory is keyed by (`history/record.ts`).
  const repo = parsed.allRepos ? undefined : parsed.repo ?? workspaceRepo(deps.cwd ?? process.cwd());

  if (parsed.verb === "sql") {
    const answer = querySql(deps.dbFile ?? dbPath(), parsed.tag!);
    if ("error" in answer) {
      deps.err(answer.error);
      return 2;
    }
    deps.out(JSON.stringify(answer.rows, null, 2));
    return 0;
  }

  if (parsed.verb === "similar") {
    const layer = (deps.embed ?? createEmbedLayer)();
    if (!layer) {
      deps.err(
        `no local embedding layer here, so there is nothing to be similar with. ` +
          `Keyword search always works: bough tags sql "SELECT h.cmd FROM ` +
          `command_history_fts f JOIN command_history h ON h.id = f.command_id ` +
          `WHERE f.cmd MATCH 'docker' ORDER BY h.ts DESC LIMIT 10"`,
      );
      return 1;
    }
    try {
      const rows = await layer.similar(parsed.tag!);
      deps.out(JSON.stringify(rows.slice(0, MAX_ROWS), null, 2));
      return 0;
    } catch (err) {
      deps.err(`similar failed: ${err instanceof Error ? err.message : String(err)}`);
      return 1;
    } finally {
      layer.close();
    }
  }

  if (parsed.verb === "show") {
    const rows = db.commandsForTag(parsed.tag!, {
      ...(repo === undefined ? {} : { repo }),
      limit: parsed.limit,
    });
    if (parsed.json) {
      deps.out(JSON.stringify(
        rows.map((r) => ({
          ...r,
          program: r.messageId === null ? null : db!.programForMessage(r.messageId),
        })),
        null,
        2,
      ));
    } else {
      renderShow(
        rows,
        parsed.tag!,
        now,
        deps.out,
        (id) => db!.programForMessage(id),
        parsed.program,
      );
    }
    return 0;
  }

  if (parsed.verb === "stats") {
    const rows = db.tagDiversityByDay(
      now - parsed.days * 24 * 60 * 60 * 1000,
      ...(repo === undefined ? [] : [repo]) as [string?],
    );
    if (parsed.json) deps.out(JSON.stringify(rows, null, 2));
    else renderStats(rows.slice(0, parsed.limit), deps.out);
    return 0;
  }

  // The default view is the priming note's own ranking. A repo-less (`--all`) list
  // has no project to be distinctive against, so it is scoped to the checkout —
  // there is nothing meaningful to rank "every project's tags" by.
  const scope = repo ?? workspaceRepo(deps.cwd ?? process.cwd());
  const ranked = rankedRepoTags(db, scope, now, parsed.limit);
  if (parsed.json) deps.out(JSON.stringify(ranked, null, 2));
  else renderList(ranked, scope, deps.out);
  return 0;
}

if (import.meta.main) {
  // FIRST, before anything opens a Database. `enableSqliteExtensions` is a
  // one-shot swap that must happen ahead of the first `new Database()`, and
  // `extensionsEnabled()` reports the decision without ever making it — so a
  // process that never calls this has NO vector layer, whatever is installed.
  //
  // It was missing here, and that made `bough tags similar` structurally dead on
  // every machine: the server enables extensions and writes embeddings.db, this
  // process never did, so `createEmbedLayer()` returned null and the verb always
  // answered "no local embedding layer here" over a store that was filling up
  // fine. Writes worked, reads could not.
  enableSqliteExtensions();
  const code = await runTags(process.argv.slice(2), {
    out: (l) => console.log(l),
    err: (l) => console.error(l),
  });
  process.exit(code);
}
