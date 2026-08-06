//! Port of `src/logs/timestamp.ts` — finding, parsing and removing the
//! timestamp a log line leads with.
//!
//! WHY THIS RUNS BEFORE EVERYTHING ELSE. A timestamp is the one field guaranteed
//! to differ on every single line, so leaving it in place would make every line
//! its own cluster and reduce the entire pipeline to a slow `cat`.
//!
//! ANCHORED AT THE START, DELIBERATELY. A mid-line scan turns any bare integer
//! into a candidate epoch, and log lines are full of bare integers.
//!
//! A LINE WITHOUT ONE IS NORMAL, NOT AN ERROR — build output, stack traces and
//! continuation lines have none and still cluster perfectly well.
//!
//! Pure: no clock is read. `ref_year` is passed in rather than taken from the
//! system clock so the same file analyzed twice produces the same output.
//!
//! The TS formats are anchored regexes with lookahead; the `regex` crate has no
//! lookaround, so each format is a hand-written anchored matcher instead. They
//! are cheaper than a regex here anyway — every one is a fixed digit layout.

/// What one line's prefix turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct StampedLine {
    /// Epoch milliseconds, when a timestamp was found AND parsed.
    pub when: Option<i64>,
    /// The line with the timestamp removed. The full line when there was none.
    pub rest: String,
    /// The text that was removed, for callers that want to show it.
    pub matched: Option<String>,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A three-letter word in the month slot that is not a month cannot happen — the
/// matchers only reach here on a full structural match — but returning 0 rather
/// than an error keeps a malformed line as a wrong date instead of poisoning the
/// span.
fn month_index(mon: &str) -> i64 {
    let lower = mon.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == lower).unwrap_or(0) as i64
}

/// Days from 1970-01-01 for a proleptic-Gregorian civil date (Howard Hinnant's
/// algorithm). `m` is 1-based and must already be normalized.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date.UTC(y, month0, d, h, mi, s, ms)`, including its rollover semantics —
/// an out-of-range month or day carries into the next unit rather than failing,
/// which is what the TS parses do for a malformed line.
pub(crate) fn date_utc_ms(y: i64, month0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> i64 {
    let y = y + month0.div_euclid(12);
    let m = month0.rem_euclid(12) + 1;
    let days = days_from_civil(y, m, 1) + (d - 1);
    days * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms
}

/// `+0530`, `+05:30` or `Z` as milliseconds to SUBTRACT from a UTC-assembled
/// time. Getting this backwards is the classic bug and it is invisible in the
/// output — the span is simply wrong by hours.
fn offset_ms(off: Option<&str>) -> i64 {
    let off = match off {
        None | Some("Z") | Some("") => return 0,
        Some(o) => o,
    };
    let b: Vec<char> = off.chars().collect();
    if b.is_empty() {
        return 0;
    }
    let sign = match b[0] {
        '+' => 1,
        '-' => -1,
        _ => return 0,
    };
    let digits: Vec<char> = b[1..].iter().copied().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.iter().all(|c| c.is_ascii_digit()) {
        return 0;
    }
    let hh: i64 = format!("{}{}", digits[0], digits[1]).parse().unwrap_or(0);
    let mm: i64 = format!("{}{}", digits[2], digits[3]).parse().unwrap_or(0);
    sign * (hh * 60 + mm) * 60_000
}

// ---------------------------------------------------------------------------
// Primitive scanners
// ---------------------------------------------------------------------------

fn digits_at(s: &[char], i: usize, n: usize) -> Option<i64> {
    if i + n > s.len() {
        return None;
    }
    let mut v: i64 = 0;
    for c in &s[i..i + n] {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (*c as i64 - '0' as i64);
    }
    Some(v)
}

fn ch(s: &[char], i: usize) -> Option<char> {
    s.get(i).copied()
}

/// `Number((frac + "000").slice(0, 3))` — `.1` is 100ms, not 1ms.
fn frac_to_ms(frac: &str) -> i64 {
    let padded = format!("{frac}000");
    padded[..3].parse().unwrap_or(0)
}

/// A run of 1..=max digits starting at `i`, as (text, end).
fn digit_run(s: &[char], i: usize, max: usize) -> Option<(String, usize)> {
    let mut j = i;
    while j < s.len() && j - i < max && s[j].is_ascii_digit() {
        j += 1;
    }
    if j == i {
        None
    } else {
        Some((s[i..j].iter().collect(), j))
    }
}

/// The optional zone suffix: `Z` or `[+-]\d{2}:?\d{2}`.
fn zone_at(s: &[char], i: usize) -> (Option<String>, usize) {
    match ch(s, i) {
        Some('Z') => (Some("Z".to_string()), i + 1),
        Some('+') | Some('-') => {
            let sign = s[i];
            let Some(hh) = digits_at(s, i + 1, 2) else {
                return (None, i);
            };
            let mut j = i + 3;
            if ch(s, j) == Some(':') {
                j += 1;
            }
            let Some(mm) = digits_at(s, j, 2) else {
                return (None, i);
            };
            (Some(format!("{sign}{hh:02}{mm:02}")), j + 2)
        }
        _ => (None, i),
    }
}

// ---------------------------------------------------------------------------
// The formats, in match order
// ---------------------------------------------------------------------------

/// ISO 8601 / RFC 3339, and the common variant using a space instead of `T`.
fn try_iso(s: &[char], _year: i64) -> Option<(i64, usize)> {
    let y = digits_at(s, 0, 4)?;
    if ch(s, 4) != Some('-') {
        return None;
    }
    let mo = digits_at(s, 5, 2)?;
    if ch(s, 7) != Some('-') {
        return None;
    }
    let d = digits_at(s, 8, 2)?;
    match ch(s, 10) {
        Some('T') | Some(' ') => {}
        _ => return None,
    }
    let h = digits_at(s, 11, 2)?;
    if ch(s, 13) != Some(':') {
        return None;
    }
    let mi = digits_at(s, 14, 2)?;
    if ch(s, 16) != Some(':') {
        return None;
    }
    let sec = digits_at(s, 17, 2)?;
    let mut j = 19;
    let mut ms = 0i64;
    if matches!(ch(s, j), Some('.') | Some(',')) {
        if let Some((frac, end)) = digit_run(s, j + 1, 9) {
            ms = frac_to_ms(&frac);
            j = end;
        }
    }
    let (off, end) = zone_at(s, j);
    let base = date_utc_ms(y, mo - 1, d, h, mi, sec, ms);
    Some((base - offset_ms(off.as_deref()), end))
}

/// Apache / nginx access logs: `15/Jan/2024:14:22:01 +0000`.
fn try_apache(s: &[char], _year: i64) -> Option<(i64, usize)> {
    let d = digits_at(s, 0, 2)?;
    if ch(s, 2) != Some('/') {
        return None;
    }
    let c0 = ch(s, 3)?;
    let c1 = ch(s, 4)?;
    let c2 = ch(s, 5)?;
    if !(c0.is_ascii_uppercase() && c1.is_ascii_lowercase() && c2.is_ascii_lowercase()) {
        return None;
    }
    let mon: String = [c0, c1, c2].iter().collect();
    if ch(s, 6) != Some('/') {
        return None;
    }
    let y = digits_at(s, 7, 4)?;
    if ch(s, 11) != Some(':') {
        return None;
    }
    let h = digits_at(s, 12, 2)?;
    if ch(s, 14) != Some(':') {
        return None;
    }
    let mi = digits_at(s, 15, 2)?;
    if ch(s, 17) != Some(':') {
        return None;
    }
    let sec = digits_at(s, 18, 2)?;
    let mut end = 20;
    let mut off: Option<String> = None;
    // `(?:\s([+-]\d{4}))?` — one whitespace character then a four-digit offset.
    if matches!(ch(s, end), Some(c) if c.is_whitespace()) {
        let sign = ch(s, end + 1);
        if matches!(sign, Some('+') | Some('-')) {
            if let Some(_v) = digits_at(s, end + 2, 4) {
                off = Some(s[end + 1..end + 6].iter().collect());
                end += 6;
            }
        }
    }
    let base = date_utc_ms(y, month_index(&mon), d, h, mi, sec, 0);
    Some((base - offset_ms(off.as_deref()), end))
}

/// syslog / BSD: `Jan 15 14:22:01`, day space-padded to width two. No year in
/// the format at all, which is why `ref_year` exists.
fn try_syslog(s: &[char], year: i64) -> Option<(i64, usize)> {
    let c0 = ch(s, 0)?;
    let c1 = ch(s, 1)?;
    let c2 = ch(s, 2)?;
    if !(c0.is_ascii_uppercase() && c1.is_ascii_lowercase() && c2.is_ascii_lowercase()) {
        return None;
    }
    let mon: String = [c0, c1, c2].iter().collect();
    // `\s{1,2}` — greedy.
    let mut i = 3;
    let mut spaces = 0;
    while spaces < 2 && matches!(ch(s, i), Some(c) if c.is_whitespace()) {
        i += 1;
        spaces += 1;
    }
    if spaces == 0 {
        return None;
    }
    // `(\d{1,2})` greedy, then a single space.
    let (day_text, mut j) = digit_run(s, i, 2)?;
    let mut day_text = day_text;
    if ch(s, j) != Some(' ') {
        // Backtrack the greedy `\d{1,2}` to one digit, and the `\s{1,2}` to one.
        if day_text.len() == 2 {
            day_text.pop();
            j -= 1;
            if ch(s, j) != Some(' ') {
                return None;
            }
        } else {
            return None;
        }
    }
    let d: i64 = day_text.parse().ok()?;
    let h = digits_at(s, j + 1, 2)?;
    if ch(s, j + 3) != Some(':') {
        return None;
    }
    let mi = digits_at(s, j + 4, 2)?;
    if ch(s, j + 6) != Some(':') {
        return None;
    }
    let sec = digits_at(s, j + 7, 2)?;
    Some((
        date_utc_ms(year, month_index(&mon), d, h, mi, sec, 0),
        j + 9,
    ))
}

/// Bare date and time, no `T` and no zone: `2024-01-15 14:22:01`. Read as UTC,
/// because guessing the writer's zone from the host's would make the same file
/// analyze differently on two machines.
fn try_plain(s: &[char], _year: i64) -> Option<(i64, usize)> {
    let y = digits_at(s, 0, 4)?;
    let sep = ch(s, 4)?;
    if sep != '-' && sep != '/' {
        return None;
    }
    let mo = digits_at(s, 5, 2)?;
    let sep2 = ch(s, 7)?;
    if sep2 != '-' && sep2 != '/' {
        return None;
    }
    let d = digits_at(s, 8, 2)?;
    if !matches!(ch(s, 10), Some(c) if c.is_whitespace()) {
        return None;
    }
    let h = digits_at(s, 11, 2)?;
    if ch(s, 13) != Some(':') {
        return None;
    }
    let mi = digits_at(s, 14, 2)?;
    let mut end = 16;
    let mut sec = 0i64;
    if ch(s, 16) == Some(':') {
        if let Some(v) = digits_at(s, 17, 2) {
            sec = v;
            end = 19;
        }
    }
    Some((date_utc_ms(y, mo - 1, d, h, mi, sec, 0), end))
}

/// Epoch, seconds or milliseconds. Width is the only signal distinguishing the
/// two and it is a reliable one for any plausible log. The trailing boundary is
/// what keeps this safe: without it a 13-digit request ID would parse as a date.
fn try_epoch(s: &[char], _year: i64) -> Option<(i64, usize)> {
    // `^(\d{10})(?:\.(\d{1,6}))?(?![\d.])`
    if let Some(secs) = digits_at(s, 0, 10) {
        let mut end = 10;
        let mut ms = 0i64;
        if ch(s, 10) == Some('.') {
            if let Some((frac, fend)) = digit_run(s, 11, 6) {
                ms = frac_to_ms(&frac);
                end = fend;
            }
        }
        let ok = match ch(s, end) {
            None => true,
            Some(c) => !c.is_ascii_digit() && c != '.',
        };
        if ok {
            return Some((secs * 1000 + ms, end));
        }
    }
    // `^(\d{13})(?!\d)`
    if let Some(msv) = digits_at(s, 0, 13) {
        let ok = match ch(s, 13) {
            None => true,
            Some(c) => !c.is_ascii_digit(),
        };
        if ok {
            return Some((msv, 13));
        }
    }
    None
}

type Format = fn(&[char], i64) -> Option<(i64, usize)>;

/// The formats, in match order. The first one that matches at position zero
/// wins, so more specific patterns are listed before the ones they would be
/// swallowed by.
const FORMATS: [Format; 5] = [try_iso, try_apache, try_syslog, try_plain, try_epoch];

/// Strip the leading timestamp from one line.
///
/// `ref_year` supplies the year for formats that omit it (syslog). It defaults
/// to 1970 rather than the current year so that a caller who forgets to pass one
/// gets an obviously wrong date instead of a subtly wrong one.
pub fn strip_timestamp(line: &str, ref_year: i64) -> StampedLine {
    let chars: Vec<char> = line.chars().collect();
    // Leading whitespace and one opening bracket are consumed first: `[2024-01-15
    // 14:22:01] msg` is extremely common and none of the anchored formats would
    // match through the bracket.
    let mut prefix_len = 0usize;
    while prefix_len < chars.len() && chars[prefix_len].is_whitespace() {
        prefix_len += 1;
    }
    let bracket = match chars.get(prefix_len) {
        Some('[') => {
            prefix_len += 1;
            Some('[')
        }
        Some('(') => {
            prefix_len += 1;
            Some('(')
        }
        _ => None,
    };
    let body = &chars[prefix_len..];

    for f in FORMATS {
        let Some((when, len)) = f(body, ref_year) else {
            continue;
        };
        let mut end = prefix_len + len;
        // Consume the bracket we opened with, plus the separator after it.
        if let Some(open) = bracket {
            let close = if open == '[' { ']' } else { ')' };
            if chars.get(end) == Some(&close) {
                end += 1;
            }
        }
        let rest: String = chars[end..]
            .iter()
            .collect::<String>()
            .trim_start_matches([' ', '\t', '\n', '\r', ':', '—', '-'])
            .to_string();
        return StampedLine {
            // `Number.isFinite(when)` in TS; an i64 result is finite by
            // construction, so the guard collapses.
            when: Some(when),
            rest,
            matched: Some(chars[..end].iter().collect()),
        };
    }
    StampedLine {
        when: None,
        rest: line.to_string(),
        matched: None,
    }
}

#[cfg(test)]
mod tests {
    //! The timestamp half of `src/logs/mask.test.ts`.
    use super::*;

    fn utc(y: i64, mo0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> i64 {
        date_utc_ms(y, mo0, d, h, mi, s, ms)
    }

    #[test]
    fn parses_iso_8601_with_fraction_and_zone() {
        let r = strip_timestamp("2024-01-15T14:22:01.100Z INFO up", 1970);
        assert_eq!(r.rest, "INFO up");
        assert_eq!(r.when, Some(utc(2024, 0, 15, 14, 22, 1, 100)));
    }

    #[test]
    fn applies_the_utc_offset_rather_than_ignoring_it() {
        let utc0 = strip_timestamp("2024-01-15T14:22:01Z x", 1970)
            .when
            .unwrap();
        let plus = strip_timestamp("2024-01-15T19:52:01+05:30 x", 1970)
            .when
            .unwrap();
        assert_eq!(plus, utc0);
        let minus = strip_timestamp("2024-01-15T09:22:01-05:00 x", 1970)
            .when
            .unwrap();
        assert_eq!(minus, utc0);
    }

    #[test]
    fn pads_fractional_seconds_instead_of_parsing_them_as_a_decimal() {
        assert_eq!(
            strip_timestamp("2024-01-15T00:00:00.1Z x", 1970).when,
            Some(utc(2024, 0, 15, 0, 0, 0, 100))
        );
        assert_eq!(
            strip_timestamp("2024-01-15T00:00:00.123456Z x", 1970).when,
            Some(utc(2024, 0, 15, 0, 0, 0, 123))
        );
    }

    #[test]
    fn handles_the_bracketed_apache_and_syslog_forms() {
        let b = strip_timestamp("[2024-01-15 14:22:01] boot", 1970);
        assert_eq!(b.rest, "boot");
        assert_eq!(b.when, Some(utc(2024, 0, 15, 14, 22, 1, 0)));

        let a = strip_timestamp("15/Jan/2024:14:22:01 +0000 GET /", 1970);
        assert_eq!(a.when, Some(utc(2024, 0, 15, 14, 22, 1, 0)));

        let s = strip_timestamp("Jan 15 14:22:01 host sshd: in", 2024);
        assert_eq!(s.rest, "host sshd: in");
        assert_eq!(s.when, Some(utc(2024, 0, 15, 14, 22, 1, 0)));
    }

    #[test]
    fn apache_offset_is_applied() {
        let z = strip_timestamp("15/Jan/2024:14:22:01 +0000 GET /", 1970)
            .when
            .unwrap();
        let plus = strip_timestamp("15/Jan/2024:19:52:01 +0530 GET /", 1970)
            .when
            .unwrap();
        assert_eq!(plus, z);
    }

    #[test]
    fn syslog_single_digit_day() {
        let s = strip_timestamp("Jan  5 14:22:01 host x", 2024);
        assert_eq!(s.when, Some(utc(2024, 0, 5, 14, 22, 1, 0)));
        assert_eq!(s.rest, "host x");
    }

    #[test]
    fn tells_epoch_seconds_from_milliseconds_by_width() {
        assert_eq!(
            strip_timestamp("1705328521 up", 1970).when,
            Some(1_705_328_521_000)
        );
        assert_eq!(
            strip_timestamp("1705328521123 up", 1970).when,
            Some(1_705_328_521_123)
        );
    }

    #[test]
    fn does_not_read_a_long_id_as_an_epoch() {
        // Without the trailing-boundary guard a 16-digit request id becomes a
        // date and the analysis reports a span of centuries.
        let r = strip_timestamp("1705328521123456 request done", 1970);
        assert_eq!(r.when, None);
        assert_eq!(r.rest, "1705328521123456 request done");
    }

    #[test]
    fn leaves_an_unstamped_line_entirely_alone() {
        let r = strip_timestamp("  at Object.<anonymous> (/app/index.js:1:1)", 1970);
        assert_eq!(r.when, None);
        assert_eq!(r.rest, "  at Object.<anonymous> (/app/index.js:1:1)");
    }

    #[test]
    fn plain_form_without_seconds() {
        let r = strip_timestamp("2024/01/15 14:22 started", 1970);
        assert_eq!(r.when, Some(utc(2024, 0, 15, 14, 22, 0, 0)));
        assert_eq!(r.rest, "started");
    }

    #[test]
    fn ref_year_defaults_to_something_obviously_wrong() {
        // 1970 rather than "silently last year": the mistake must be visible.
        let r = strip_timestamp("Jan 15 14:22:01 host x", 1970);
        assert_eq!(r.when, Some(utc(1970, 0, 15, 14, 22, 1, 0)));
    }
}
