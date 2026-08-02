/**
 * Stage one of clustering: replace every value-shaped span in a line with a typed
 * placeholder, and hand back the values that were removed.
 *
 * THE IDEA, from CLP (Rodrigues et al., OSDI 2021). Two lines that differ only in
 * their values are the same log statement — the same `printf` in the same function
 * — and the cheapest way to recognize that is to delete the values. What is left is
 * the "logtype", a string that is byte-identical for every execution of that
 * statement. Clustering on logtypes rather than on raw lines means the expensive
 * similarity work in `drain.ts` runs over thousands of distinct logtypes instead of
 * millions of lines, and it means the common case — a statement whose values are
 * all recognizable — needs no similarity work at all, just a hash lookup.
 *
 * TYPED, NOT ANONYMOUS. A placeholder carries its kind (`<ipv4>`, `<duration>`)
 * rather than being a bare `<*>`. This costs nothing at match time and buys two
 * things. It separates statements that a shapeless mask would merge, since
 * `connect to <*>` from an IP and from a hostname are genuinely different lines.
 * And it tells the accumulator how to treat the slot before it has seen any values,
 * so a duration slot gets quantiles and a UUID slot does not — a decision that
 * would otherwise need a second pass over data already discarded.
 *
 * ONE LEFT-TO-RIGHT SCAN, FIRST ALTERNATIVE WINS. Every kind lives in a single
 * combined regex and the line is scanned once. Running the patterns one after
 * another over the whole string instead would let a later pass match inside a
 * placeholder an earlier one wrote — the `<int>` in a template is itself a word,
 * and a naive path pattern would happily consume it. Alternation order is therefore
 * load-bearing and is documented at each entry: the specific must precede the
 * general it would otherwise be eaten by.
 *
 * ORDER MATTERS MOST FOR NUMBERS. Every kind below that contains digits — the
 * address, the duration, the size, the identifier — is a special case of "there is
 * a number here", and `int` is listed last precisely so that it only claims digits
 * nothing else wanted. Put `int` first and the analysis degrades to counting
 * integers, which is the failure mode that makes a variable slot report request IDs
 * under the heading `duration`.
 *
 * Pure and allocation-light: one regex, one pass, no clock, no filesystem.
 */
import type { VarKind, VarValue } from "./types.ts";

/** A line reduced to structure, plus the values that were removed. */
export interface Masked {
  /** The line with each value replaced by `<kind>`. Identical across executions. */
  logtype: string;
  /** The removed values, left to right, aligned with the placeholders in `logtype`. */
  values: VarValue[];
}

/**
 * "Not in the middle of a word", as a lookbehind and a lookahead.
 *
 * EVERY KIND THAT CONTAINS DIGITS NEEDS THESE, and leaving them off is the single
 * most damaging mistake this module can make — damaging because it does not look
 * like a failure. A session id like `a107b3f` contains the substring `107b`, which
 * is a perfectly good byte size, and `c0d9e` contains `0d`, a perfectly good
 * duration. Without a left fence the masker carves identifiers into fragments and
 * emits `a<bytes><int>f`, which then fails to match the same statement's other
 * lines and splits one pattern into several — each with a `duration` slot whose
 * quantiles are computed over pieces of random hex. The report still renders, still
 * looks authoritative, and reports request ids under the heading `duration`.
 *
 * Word characters only: a preceding `.` or `:` or `=` must NOT block a match, since
 * `status=200`, `:5432` and `1.5` are exactly the shapes worth catching.
 */
const L = `(?<![A-Za-z0-9_])`;
const R = `(?![A-Za-z0-9_])`;

/**
 * The kinds, in alternation order. Each entry's `re` source is spliced into one
 * combined pattern with a named group, and the group that matched names the kind.
 *
 * Names must be valid JS identifiers, so they double as the group names.
 */
const KINDS: { kind: VarKind; re: string; why: string }[] = [
  {
    kind: "quoted",
    re: `"[^"\\n]*"|'[^'\\n]*'`,
    why: "First, because a quoted string may contain anything — a path, a number, an IP — and those are values of the message, not of the log statement. Matching inside quotes would split one variable into five.",
  },
  {
    kind: "uuid",
    re: `${L}[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}${R}`,
    why: "Before `hex` and `int`, both of which would claim its first segment and leave the rest as debris.",
  },
  {
    kind: "url",
    re: `[a-zA-Z][a-zA-Z0-9+.\\-]*://[^\\s"'<>\\]\\)]+`,
    why: "Before `path`, `ipv4` and `int`, all of which appear inside a URL. The whole URL is one value.",
  },
  {
    kind: "timestamp",
    re: `\\d{4}-\\d{2}-\\d{2}[T ]\\d{2}:\\d{2}:\\d{2}(?:[.,]\\d{1,9})?(?:Z|[+-]\\d{2}:?\\d{2})?`,
    why: "A timestamp INSIDE the message (a deadline, an expiry) — the leading one is already gone. Before `int`, which would shred it into six numbers.",
  },
  {
    kind: "ipv6",
    re: `(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:(?:[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4}){0,6})?`,
    why: "Deliberately conservative: either all eight groups, or a `::` elision. A permissive IPv6 pattern matches `14:22:01` and turns clock times into addresses.",
  },
  {
    kind: "ipv4",
    re: `(?<![\\d.])(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)(?![\\d.])`,
    why: "Octet-range-checked AND fenced by digit boundaries, so a version string like `1.2.3.4000` is not an address — without the trailing fence the pattern happily matches `1.2.3.4` and abandons the `000`. Before `float`, which would take `10.0` and leave `.1.15`.",
  },
  {
    kind: "bytes",
    re: `${L}\\d+(?:\\.\\d+)?\\s?(?:[KMGTP]i?B|[kmgtp]?[bB])${R}`,
    why: "Before `duration`, so the `B` in `5MB` is not read as a bare unit, and before the number kinds.",
  },
  {
    kind: "duration",
    re: `${L}\\d+(?:\\.\\d+)?(?:ns|µs|us|ms|s|m|h|d)${R}`,
    why: "Before `float`/`int`, which would strand the unit as a word and destroy the template. No space allowed before the unit: `5 m` is far more often `5 metres` than five minutes.",
  },
  {
    kind: "hex",
    re: `${L}0[xX][0-9a-fA-F]+${R}|${L}[0-9a-fA-F]{8,}${R}`,
    why: "Bare hex needs 8+ chars and nothing more. Requiring a digit as well — to keep words like `deadbeef` out — turns out to be the wrong trade: an all-letter id such as `bebbccce` is then left unmasked, indexes literally in the clustering tree, and becomes its own singleton pattern. On a 500k-line log that produced a hundred junk patterns that should have been one. Length alone is a good enough filter, because English words of 8+ letters drawn only from a-f essentially do not exist, and `deadbeef` genuinely IS a hex constant. Before `int`, which would claim a digit-leading id.",
  },
  {
    kind: "path",
    re: `(?:~|\\.{1,2})?(?:/[\\w.@+\\-]+){2,}/?`,
    why: "Two or more segments, so a lone `/` between words is not a path. Before the number kinds, which would carve up `/var/log/app2.log`.",
  },
  {
    kind: "float",
    re: `${L}-?\\d+\\.\\d+${R}`,
    why: "Before `int`, which would take the whole part and leave a stray fraction.",
  },
  {
    kind: "int",
    re: `${L}-?\\d+${R}`,
    why: "Last. Claims only the digits no more specific kind wanted.",
  },
];

/**
 * All kinds in one alternation, each in a named group.
 *
 * Built once at module load. Rebuilding per line was measurably the single
 * most expensive thing the pipeline did — regex compilation dominated the profile
 * on a million-line file, above the tree walk it exists to feed.
 */
const COMBINED = new RegExp(KINDS.map((k) => `(?<${k.kind}>${k.re})`).join("|"), "g");

/** Milliseconds per duration unit, for normalizing a slot's quantiles. */
const DURATION_MS: Record<string, number> = {
  ns: 1e-6,
  µs: 1e-3,
  us: 1e-3,
  ms: 1,
  s: 1000,
  m: 60000,
  h: 3600000,
  d: 86400000,
};

/** Bytes per size unit. Binary and decimal prefixes are both spelled the same way in logs. */
const BYTE_SCALE: Record<string, number> = {
  b: 1,
  kb: 1024,
  kib: 1024,
  mb: 1024 ** 2,
  mib: 1024 ** 2,
  gb: 1024 ** 3,
  gib: 1024 ** 3,
  tb: 1024 ** 4,
  tib: 1024 ** 4,
  pb: 1024 ** 5,
  pib: 1024 ** 5,
};

/**
 * The comparable magnitude for a value, in the kind's base unit.
 *
 * Durations become milliseconds and sizes become bytes so that a slot holding both
 * `1.5s` and `900ms` sorts correctly. Quantiles over the bare numerals would rank
 * 900 above 1.5 and report the fast case as the slow one — which is not a rounding
 * error but an inverted answer, and it is the kind a reader would act on.
 */
function magnitude(kind: VarKind, raw: string): number | undefined {
  if (kind === "int" || kind === "float") {
    const n = Number(raw);
    return Number.isFinite(n) ? n : undefined;
  }
  if (kind === "duration") {
    const m = /^(-?\d+(?:\.\d+)?)(ns|µs|us|ms|s|m|h|d)$/.exec(raw);
    if (!m) return undefined;
    return Number(m[1]) * (DURATION_MS[m[2] as string] as number);
  }
  if (kind === "bytes") {
    const m = /^(-?\d+(?:\.\d+)?)\s?([KMGTP]i?B|[kmgtp]?[bB])$/.exec(raw);
    if (!m) return undefined;
    const scale = BYTE_SCALE[(m[2] as string).toLowerCase()];
    return scale === undefined ? undefined : Number(m[1]) * scale;
  }
  return undefined;
}

/**
 * Mask one line.
 *
 * The input should already have had its leading timestamp removed by
 * `stripTimestamp` — this function will happily mask one that is still there, but
 * as a `<timestamp>` variable rather than as the line's clock, and the temporal
 * analysis would get nothing.
 */
export function mask(line: string): Masked {
  const values: VarValue[] = [];
  let out = "";
  let last = 0;
  // `lastIndex` is reset explicitly rather than trusted: the regex is a module
  // singleton with the `g` flag, so a previous call that returned early would leave
  // the cursor mid-string and silently skip the head of this line.
  COMBINED.lastIndex = 0;
  for (let m = COMBINED.exec(line); m !== null; m = COMBINED.exec(line)) {
    // A zero-width match cannot happen with these alternatives, but if a future
    // kind introduced one this loop would spin forever. Cheaper to rule out.
    if (m[0].length === 0) {
      COMBINED.lastIndex++;
      continue;
    }
    const groups = m.groups as Record<string, string | undefined>;
    // Exactly one group is defined per match; find which. KINDS is short enough
    // that a scan beats maintaining a reverse index.
    const hit = KINDS.find((k) => groups[k.kind] !== undefined);
    if (!hit) continue;
    out += line.slice(last, m.index);
    // Recorded BEFORE the placeholder is appended, so `at` points at its `<`.
    const at = out.length;
    out += `<${hit.kind}>`;
    const num = magnitude(hit.kind, m[0]);
    values.push(
      num === undefined ? { kind: hit.kind, raw: m[0], at } : { kind: hit.kind, raw: m[0], num, at },
    );
    last = m.index + m[0].length;
  }
  out += line.slice(last);
  return { logtype: out, values };
}

/** The kinds and the reason each sits where it does. Rendered by `bough patterns --explain`. */
export function kindOrder(): { kind: VarKind; why: string }[] {
  return KINDS.map((k) => ({ kind: k.kind, why: k.why }));
}
