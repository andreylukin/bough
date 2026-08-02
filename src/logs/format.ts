/**
 * The three renderings of an `Analysis`, and the shared vocabulary between them.
 *
 * ONE MODULE, THREE FORMATS, because they are three views of one decision about
 * what matters — and split across three files they drift, which shows up as a
 * `--json` field the `--llm` view stopped reporting and nobody noticed. What
 * differs between them is genuinely presentational; what is worth SAYING is the
 * same, and it lives in `analyze.ts` upstream of all three.
 *
 * WHO EACH ONE IS FOR:
 *
 *   --llm    a language model reading the output inside a turn. Optimizes for
 *            tokens and for unambiguous structure. This is the default when stdout
 *            is not a terminal, because that is overwhelmingly the case where
 *            something else is consuming it.
 *   --human  a person at a terminal. Optimizes for scanning: colour, aligned
 *            columns, a bar that shows shape at a glance.
 *   --json   a program. Optimizes for stability. It is `Analysis` verbatim, so the
 *            contract is `types.ts` and nothing is invented at render time.
 *
 * WHAT NONE OF THEM DO. No format carries a footer advertising anything, and this
 * is a deliberate constraint rather than an oversight. The `--llm` output is fed
 * into a model's context window on every invocation; every line that is not about
 * the log is a line that displaces one that is, and a marketing sentence in that
 * position is a tax charged on someone else's attention. If this ever grows a
 * "learn more" line, it belongs in `--help`.
 *
 * NUMBERS ARE NEVER SILENTLY ROUNDED INTO A LIE. Where a value is approximate, the
 * text says so (`~1,847`); where a ranking is untrustworthy it is omitted rather
 * than shown with a caveat nobody reads; where the analysis was truncated the
 * header says that too. The whole tool is an argument that a summary can be trusted,
 * and it only holds if the summary admits what it does not know.
 */
import { fmt } from "./anomaly.ts";
import type { Analysis, Pattern, Severity, VarSummary } from "./types.ts";

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/** `1234567` → `1,234,567`. Used everywhere a count is shown to a person. */
function n(v: number): string {
  return v.toLocaleString("en-US");
}

/** A share as a percentage with just enough precision to distinguish small ones. */
function pct(share: number): string {
  const p = share * 100;
  if (p >= 10) return `${p.toFixed(0)}%`;
  if (p >= 1) return `${p.toFixed(1)}%`;
  return `${p.toFixed(2)}%`;
}

/** An epoch millisecond as a compact UTC stamp. */
function stamp(ms: number): string {
  return new Date(ms).toISOString().replace("T", " ").replace(/\.\d+Z$/, "Z");
}

/**
 * One variable slot as a single line of text, without any leading indent.
 *
 * Shared by `--llm` and `--human` because deciding WHICH facts about a slot are
 * worth a line is the substantive choice, and it should not be made twice. The
 * order is fixed — identity, then spread, then magnitude — so a reader scanning a
 * column of these finds the same thing in the same place every time.
 */
function slotLine(v: VarSummary): string {
  const parts: string[] = [`slot ${v.slot}`, v.kind];

  if (v.kind === "id") {
    // For an identifier the only interesting fact is that it does not repeat, and
    // the top values were suppressed upstream precisely so nothing here implies
    // otherwise.
    parts.push(`~${n(v.unique)} distinct / ${n(v.count)}`);
  } else if (v.unique === 1 && v.top?.[0]) {
    parts.push(`always ${v.top[0].value}`);
  } else {
    parts.push(`${n(v.unique)} unique`);
    if (v.top && v.top.length > 0) {
      parts.push(v.top.map((t) => `${t.value} (${pct(t.share)})`).join(", "));
    }
  }

  if (v.numeric) {
    const u = v.numeric.unit;
    parts.push(
      `p50=${fmt(v.numeric.p50, u)} p90=${fmt(v.numeric.p90, u)} p99=${fmt(v.numeric.p99, u)} max=${fmt(v.numeric.max, u)}`,
    );
  }
  return parts.join("  ");
}

// ---------------------------------------------------------------------------
// LLM
// ---------------------------------------------------------------------------

/**
 * Compact markdown, ordered so the first thing read is the most consequential.
 *
 * PROBLEMS FIRST IS THE WHOLE LAYOUT. A model reads top to bottom and weights early
 * content more heavily, so putting a 96%-of-traffic INFO pattern first — which
 * sorting by volume would do — spends the most valuable position in the context on
 * the least actionable fact in the file. Patterns arrive pre-ranked by severity from
 * `analyze.ts`; this function's job is not to re-sort but to make the ranking
 * legible, which it does by splitting at the severity boundary and labelling both
 * halves.
 *
 * EXAMPLES ARE CAPPED HARD. One real line per pattern, and only for the severe ones.
 * Raw log lines are the single most token-expensive thing that can appear here and
 * their marginal value drops off a cliff after the first — the second and third
 * mostly re-demonstrate that the template is accurate, which the template already
 * showed.
 */
export function toLlm(a: Analysis): string {
  const out: string[] = [];

  const header = [`# ${n(a.lines)} lines → ${n(a.patternCount)} patterns`];
  if (a.timeSpan) header.push(`span ${stamp(a.timeSpan.from)} … ${stamp(a.timeSpan.to)}`);
  if (a.patterns.length < a.patternCount) header.push(`showing top ${a.patterns.length}`);
  out.push(header.join(" · "));
  if (a.truncated) {
    out.push(
      "> NOTE: the cluster cap was reached and rare patterns were evicted; counts are lower bounds.",
    );
  }
  out.push("");

  const severe = a.patterns.filter((p) => p.severity === "error" || p.severity === "fatal");
  const rest = a.patterns.filter((p) => p.severity !== "error" && p.severity !== "fatal");

  if (severe.length > 0) {
    out.push(`## Problems (${severe.length})`);
    out.push("");
    for (const p of severe) out.push(...llmPattern(p, true));
  }
  if (rest.length > 0) {
    out.push(`## Everything else (${rest.length})`);
    out.push("");
    for (const p of rest) out.push(...llmPattern(p, false));
  }

  if (a.correlations.length > 0) {
    out.push("## Related");
    out.push("");
    // Phrased as observations, never as causes: co-occurrence cannot distinguish
    // "A caused B" from "C caused both", and a model reading this will act on
    // whatever verb it is given.
    for (const c of a.correlations) out.push(`- ${c.detail}`);
    out.push("");
  }

  return out.join("\n").trimEnd() + "\n";
}

function llmPattern(p: Pattern, withExample: boolean): string[] {
  const out: string[] = [];
  out.push(`### #${p.id} [${p.severity.toUpperCase()}] ${n(p.count)} lines (${pct(p.share)})`);
  out.push("```");
  out.push(p.template);
  out.push("```");
  for (const v of p.vars) {
    // Slots that never varied are dropped from the LLM view. They are constants of
    // the log statement, not variables of it, and one line each is a real cost in a
    // format whose entire premise is that it is cheap to read.
    if (v.unique === 1 && !v.numeric) continue;
    out.push(`- ${slotLine(v)}`);
  }
  for (const an of p.anomalies) out.push(`- ⚠ ${an.detail}`);
  if (withExample && p.examples.length > 0) {
    out.push(`- e.g. \`${p.examples[0]}\``);
  }
  out.push("");
  return out;
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/** `Analysis` verbatim. The contract is `types.ts`; nothing is invented here. */
export function toJson(a: Analysis): string {
  return JSON.stringify(a, null, 2) + "\n";
}

// ---------------------------------------------------------------------------
// Human
// ---------------------------------------------------------------------------

const ANSI = {
  reset: "[0m",
  dim: "[2m",
  bold: "[1m",
  red: "[31m",
  yellow: "[33m",
  blue: "[34m",
  green: "[32m",
  grey: "[90m",
};

const SEVERITY_COLOUR: Record<Severity, string> = {
  fatal: ANSI.red + ANSI.bold,
  error: ANSI.red,
  warn: ANSI.yellow,
  info: ANSI.blue,
  debug: ANSI.grey,
};

/**
 * Terminal output for a person.
 *
 * `colour` is a parameter rather than something detected here, because detection
 * needs `process.stdout.isTTY` and this module is pure — the CLI decides, this
 * renders. That also makes the no-colour path testable by passing `false` instead
 * of by faking a TTY.
 */
export function toHuman(a: Analysis, colour: boolean, width = 80): string {
  const c = (code: string, s: string) => (colour ? code + s + ANSI.reset : s);
  const out: string[] = [];

  const head = `${n(a.lines)} lines → ${n(a.patternCount)} patterns`;
  const reduction = a.lines > 0 ? 1 - a.patternCount / a.lines : 0;
  out.push(c(ANSI.bold, head) + c(ANSI.dim, `  (${pct(reduction)} reduction)`));
  if (a.timeSpan) {
    out.push(c(ANSI.dim, `${stamp(a.timeSpan.from)} … ${stamp(a.timeSpan.to)}`));
  }
  if (a.truncated) {
    out.push(c(ANSI.yellow, "cluster cap reached — rare patterns evicted, counts are lower bounds"));
  }
  out.push("");

  // The bar is scaled to the LARGEST pattern shown, not to the total, so the
  // smaller patterns remain visible. Scaled to the total, a log with one dominant
  // INFO pattern renders every other bar as a single character and the column
  // stops carrying information.
  const peak = Math.max(1, ...a.patterns.map((p) => p.count));
  const barWidth = Math.max(10, Math.min(24, width - 56));

  for (const p of a.patterns) {
    const sev = p.severity.toUpperCase().padEnd(5);
    const filled = Math.max(1, Math.round((p.count / peak) * barWidth));
    const bar = "█".repeat(filled) + c(ANSI.dim, "·".repeat(barWidth - filled));
    const head2 = `${c(ANSI.dim, `#${String(p.id).padStart(2)}`)} ${c(SEVERITY_COLOUR[p.severity], sev)} ${bar} ${n(p.count).padStart(9)} ${c(ANSI.dim, `(${pct(p.share)})`)}`;
    out.push(head2);
    out.push(`    ${c(ANSI.bold, p.template)}`);
    for (const v of p.vars) {
      if (v.unique === 1 && !v.numeric) continue;
      out.push(c(ANSI.dim, `      ${slotLine(v)}`));
    }
    for (const an of p.anomalies) out.push(`      ${c(ANSI.yellow, "⚠")} ${an.detail}`);
    if (p.examples.length > 0) {
      out.push(c(ANSI.grey, `      e.g. ${truncate(p.examples[0] as string, width - 12)}`));
    }
    out.push("");
  }

  if (a.correlations.length > 0) {
    out.push(c(ANSI.bold, "Related"));
    for (const cr of a.correlations) out.push(`  ${c(ANSI.green, "↔")} ${cr.detail}`);
    out.push("");
  }

  return out.join("\n");
}

/** Cut to width with an ellipsis, so one long line cannot wreck the layout. */
function truncate(s: string, width: number): string {
  if (width < 8 || s.length <= width) return s;
  return s.slice(0, width - 1) + "…";
}
