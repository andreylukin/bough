/**
 * Finding, parsing and removing the timestamp a log line leads with.
 *
 * WHY THIS RUNS BEFORE EVERYTHING ELSE. A timestamp is the one field guaranteed to
 * differ on every single line, so leaving it in place would make every line its own
 * cluster and reduce the entire pipeline to a slow `cat`. It is also the only field
 * whose VALUE the analysis needs as a quantity rather than as a string — the time
 * span in the header, the bucket a line falls into, the spike detector's whole
 * basis. So it is not merely masked like other variables; it is parsed.
 *
 * ANCHORED AT THE START, DELIBERATELY. Every format here is matched only at the
 * beginning of the line (after leading whitespace and an optional opening bracket),
 * even though timestamps do occur mid-line. Two reasons. A mid-line scan turns any
 * bare integer into a candidate epoch, and log lines are full of bare integers —
 * `status=200` would become a date in 1970. And the leading timestamp is the one
 * that means "when this line was written", which is the only reading the buckets
 * can use; a timestamp inside the message is data about something else and belongs
 * in a variable slot, where `mask.ts` puts it.
 *
 * A LINE WITHOUT ONE IS NORMAL, NOT AN ERROR. Build output, stack traces and
 * continuation lines have no timestamp at all, and they still cluster perfectly
 * well — they simply contribute nothing to the temporal analysis. `when` is
 * `undefined` and the caller carries on.
 *
 * Pure: no clock is read. Two-digit-year and year-less formats need a reference
 * year, and it is passed in rather than taken from `Date.now()` so that the same
 * file analyzed twice produces the same output.
 */

/** What one line's prefix turned out to be. */
export interface StampedLine {
  /** Epoch milliseconds, when a timestamp was found AND parsed. */
  when?: number;
  /** The line with the timestamp removed. The full line when there was none. */
  rest: string;
  /** The text that was removed, for callers that want to show it. */
  matched?: string;
}

/**
 * The formats, in match order. The first one that matches at position zero wins,
 * so more specific patterns are listed before the ones they would be swallowed by.
 */
const FORMATS: { name: string; re: RegExp; parse: (m: RegExpMatchArray, year: number) => number }[] =
  [
    {
      // ISO 8601 / RFC 3339, and the very common variant that uses a space instead
      // of `T`. Fractional seconds and offset both optional.
      name: "iso",
      re: /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?(Z|[+-]\d{2}:?\d{2})?/,
      parse: (m) => {
        const [, y, mo, d, h, mi, s, frac, off] = m;
        // Fractions are padded rather than parsed as a decimal: `.1` is 100ms, not
        // 1ms, and `.123456` is 123ms with the microseconds dropped.
        const ms = frac ? Number((frac + "000").slice(0, 3)) : 0;
        const base = Date.UTC(+(y as string), +(mo as string) - 1, +(d as string), +(h as string), +(mi as string), +(s as string), ms);
        return base - offsetMs(off);
      },
    },
    {
      // Apache / nginx access logs: 15/Jan/2024:14:22:01 +0000
      name: "apache",
      re: /^(\d{2})\/([A-Z][a-z]{2})\/(\d{4}):(\d{2}):(\d{2}):(\d{2})(?:\s([+-]\d{4}))?/,
      parse: (m) => {
        const [, d, mon, y, h, mi, s, off] = m;
        const base = Date.UTC(+(y as string), monthIndex(mon as string), +(d as string), +(h as string), +(mi as string), +(s as string));
        return base - offsetMs(off);
      },
    },
    {
      // syslog / BSD: `Jan 15 14:22:01`, day space-padded to width two. No year in
      // the format at all, which is why `refYear` exists.
      name: "syslog",
      re: /^([A-Z][a-z]{2})\s{1,2}(\d{1,2})\s(\d{2}):(\d{2}):(\d{2})/,
      parse: (m, year) => {
        const [, mon, d, h, mi, s] = m;
        return Date.UTC(year, monthIndex(mon as string), +(d as string), +(h as string), +(mi as string), +(s as string));
      },
    },
    {
      // Bare date and time, no `T` and no zone: `2024-01-15 14:22:01`. Read as UTC,
      // because guessing the writer's zone from the host's would make the same file
      // analyze differently on two machines.
      name: "plain",
      re: /^(\d{4})[-/](\d{2})[-/](\d{2})\s(\d{2}):(\d{2})(?::(\d{2}))?/,
      parse: (m) => {
        const [, y, mo, d, h, mi, s] = m;
        return Date.UTC(+(y as string), +(mo as string) - 1, +(d as string), +(h as string), +(mi as string), s ? +s : 0);
      },
    },
    {
      // Epoch, seconds or milliseconds, with optional fraction. Width is the only
      // signal distinguishing the two and it is a reliable one for any plausible
      // log: 10 digits is 2001-2286 in seconds, 13 is 2001-2286 in milliseconds.
      //
      // The trailing boundary is what keeps this safe. Without it a 13-digit
      // request ID would parse as a date, and `\b` alone would still accept the
      // first 13 digits of a longer number.
      name: "epoch",
      re: /^(\d{10})(?:\.(\d{1,6}))?(?![\d.])|^(\d{13})(?!\d)/,
      parse: (m) => {
        if (m[3]) return +m[3];
        const frac = m[2] ? Number((m[2] + "000").slice(0, 3)) : 0;
        return +(m[1] as string) * 1000 + frac;
      },
    },
  ];

const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

function monthIndex(mon: string): number {
  const i = MONTHS.indexOf(mon.toLowerCase());
  // A three-letter word in the month slot that is not a month cannot happen — the
  // regexes only reach `parse` on a full structural match — but returning 0 rather
  // than NaN keeps a malformed line as a wrong date instead of poisoning the span.
  return i < 0 ? 0 : i;
}

/** `+0530`, `+05:30` or `Z` as milliseconds to SUBTRACT from a UTC-assembled time. */
function offsetMs(off: string | undefined): number {
  if (!off || off === "Z") return 0;
  const m = /^([+-])(\d{2}):?(\d{2})$/.exec(off);
  if (!m) return 0;
  const mins = +(m[2] as string) * 60 + +(m[3] as string);
  return (m[1] === "-" ? -mins : mins) * 60000;
}

/**
 * Strip the leading timestamp from one line.
 *
 * `refYear` supplies the year for formats that omit it (syslog). It defaults to
 * 1970 rather than the current year so that a caller who forgets to pass one gets
 * an obviously wrong date instead of a subtly wrong one — the analysis header will
 * read `1970` and the mistake is visible, where "silently last year" is not.
 */
export function stripTimestamp(line: string, refYear = 1970): StampedLine {
  // Leading whitespace and one opening bracket are consumed first: `[2024-01-15
  // 14:22:01] msg` is extremely common and none of the anchored formats would match
  // through the bracket. The bracket is treated as part of the match so the closing
  // one, if present, is removed too.
  const open = /^(\s*)([[(])?/.exec(line);
  const prefixLen = (open?.[0] ?? "").length;
  const bracket = open?.[2];
  const body = line.slice(prefixLen);

  for (const f of FORMATS) {
    const m = f.re.exec(body);
    if (!m) continue;
    let end = prefixLen + m[0].length;
    // Consume the bracket we opened with, plus the separator after it. Leaving a
    // stray `]` in the template would be harmless but reads as a parsing bug.
    if (bracket) {
      const close = bracket === "[" ? "]" : ")";
      if (line[end] === close) end++;
    }
    const when = f.parse(m, refYear);
    return {
      when: Number.isFinite(when) ? when : undefined,
      rest: line.slice(end).replace(/^[\s:—-]+/, ""),
      matched: line.slice(0, end),
    };
  }
  return { rest: line };
}
