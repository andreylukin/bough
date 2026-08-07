//! Port of `src/logs/mask.ts` — stage one of clustering: replace every
//! value-shaped span in a line with a typed placeholder, and hand back the
//! values that were removed.
//!
//! THE IDEA, from CLP (Rodrigues et al., OSDI 2021). Two lines that differ only
//! in their values are the same log statement, and the cheapest way to recognize
//! that is to delete the values.
//!
//! TYPED, NOT ANONYMOUS. A placeholder carries its kind (`<ipv4>`,
//! `<duration>`) rather than being a bare `<*>`: it separates statements a
//! shapeless mask would merge, and it tells the accumulator how to treat the
//! slot before it has seen any values.
//!
//! ONE LEFT-TO-RIGHT SCAN, FIRST ALTERNATIVE WINS. The TS side is one combined
//! `g`-flagged alternation; the alternation ORDER is load-bearing and is
//! documented at each entry. This port keeps the identical semantics with
//! hand-written anchored matchers tried in the same order at each position,
//! because the TS patterns use lookbehind/lookahead word fences that the `regex`
//! crate cannot express — and because "check the fence characters manually at
//! the match boundaries" is what `specs/small.md` prescribes.
//!
//! ORDER MATTERS MOST FOR NUMBERS. Every kind below that contains digits is a
//! special case of "there is a number here", and `int` is listed last precisely
//! so that it only claims digits nothing else wanted.
//!
//! Pure and allocation-light: one pass, no clock, no filesystem.

use super::types::{VarKind, VarValue};

/// A line reduced to structure, plus the values that were removed.
#[derive(Debug, Clone, PartialEq)]
pub struct Masked {
    /// The line with each value replaced by `<kind>`. Identical across
    /// executions of the same statement.
    pub logtype: String,
    /// The removed values, left to right, aligned with the placeholders.
    pub values: Vec<VarValue>,
}

// ---------------------------------------------------------------------------
// Word fences
// ---------------------------------------------------------------------------
//
// "EVERY KIND THAT CONTAINS DIGITS NEEDS THESE, and leaving them off is the
// single most damaging mistake this module can make — damaging because it does
// not look like a failure." A session id like `a107b3f` contains `107b`, a
// perfectly good byte size. Word characters only: a preceding `.`, `:` or `=`
// must NOT block a match, since `status=200`, `:5432` and `1.5` are exactly the
// shapes worth catching.

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn left_fence(s: &[char], i: usize) -> bool {
    i == 0 || !is_word(s[i - 1])
}

fn right_fence(s: &[char], j: usize) -> bool {
    j >= s.len() || !is_word(s[j])
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

/// Exactly `n` hex digits starting at `i`.
fn hex_run_exact(s: &[char], i: usize, n: usize) -> bool {
    i + n <= s.len() && s[i..i + n].iter().all(|c| is_hex(*c))
}

/// The maximal run of ASCII digits starting at `i`.
fn digits_end(s: &[char], i: usize) -> usize {
    let mut j = i;
    while j < s.len() && s[j].is_ascii_digit() {
        j += 1;
    }
    j
}

// ---------------------------------------------------------------------------
// The kinds, in alternation order
// ---------------------------------------------------------------------------

/// "First, because a quoted string may contain anything — a path, a number, an
/// IP — and those are values of the message, not of the log statement."
fn m_quoted(s: &[char], i: usize) -> Option<usize> {
    let q = s[i];
    if q != '"' && q != '\'' {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() {
        if s[j] == '\n' {
            return None;
        }
        if s[j] == q {
            return Some(j + 1);
        }
        j += 1;
    }
    None
}

/// "Before `hex` and `int`, both of which would claim its first segment and
/// leave the rest as debris."
fn m_uuid(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    let widths = [8usize, 4, 4, 4, 12];
    let mut j = i;
    for (n, width) in widths.iter().enumerate() {
        if n > 0 {
            if s.get(j) != Some(&'-') {
                return None;
            }
            j += 1;
        }
        if !hex_run_exact(s, j, *width) {
            return None;
        }
        j += width;
    }
    if !right_fence(s, j) {
        return None;
    }
    Some(j)
}

/// "Before `path`, `ipv4` and `int`, all of which appear inside a URL. The whole
/// URL is one value."
fn m_url(s: &[char], i: usize) -> Option<usize> {
    if !s[i].is_ascii_alphabetic() {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && (s[j].is_ascii_alphanumeric() || matches!(s[j], '+' | '.' | '-')) {
        j += 1;
    }
    if s.get(j) != Some(&':') || s.get(j + 1) != Some(&'/') || s.get(j + 2) != Some(&'/') {
        return None;
    }
    j += 3;
    let start_body = j;
    while j < s.len()
        && !matches!(s[j], '"' | '\'' | '<' | '>' | ']' | ')')
        && !s[j].is_whitespace()
    {
        j += 1;
    }
    if j == start_body {
        return None;
    }
    Some(j)
}

/// A timestamp INSIDE the message (a deadline, an expiry) — the leading one is
/// already gone. Before `int`, which would shred it into six numbers.
fn m_timestamp(s: &[char], i: usize) -> Option<usize> {
    let d = |j: usize, n: usize| -> bool {
        j + n <= s.len() && s[j..j + n].iter().all(|c| c.is_ascii_digit())
    };
    if !d(i, 4) || s.get(i + 4) != Some(&'-') || !d(i + 5, 2) || s.get(i + 7) != Some(&'-') {
        return None;
    }
    if !d(i + 8, 2) || !matches!(s.get(i + 10), Some('T') | Some(' ')) {
        return None;
    }
    if !d(i + 11, 2) || s.get(i + 13) != Some(&':') || !d(i + 14, 2) || s.get(i + 16) != Some(&':')
    {
        return None;
    }
    if !d(i + 17, 2) {
        return None;
    }
    let mut j = i + 19;
    if matches!(s.get(j), Some('.') | Some(',')) {
        let end = digits_end(s, j + 1).min(j + 1 + 9);
        if end > j + 1 {
            j = end;
        }
    }
    match s.get(j) {
        Some('Z') => j += 1,
        Some('+') | Some('-') if d(j + 1, 2) => {
            let mut k = j + 3;
            if s.get(k) == Some(&':') {
                k += 1;
            }
            if d(k, 2) {
                j = k + 2;
            }
        }
        _ => {}
    }
    Some(j)
}

/// "Deliberately conservative: either all eight groups, or a `::` elision. A
/// permissive IPv6 pattern matches `14:22:01` and turns clock times into
/// addresses."
fn m_ipv6(s: &[char], i: usize) -> Option<usize> {
    // `(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}`
    let group = |j: usize| -> Option<usize> {
        let mut k = j;
        while k < s.len() && k - j < 4 && is_hex(s[k]) {
            k += 1;
        }
        if k == j {
            None
        } else {
            Some(k)
        }
    };
    let mut j = i;
    let mut ok = true;
    for _ in 0..7 {
        match group(j) {
            Some(k) if s.get(k) == Some(&':') => j = k + 1,
            _ => {
                ok = false;
                break;
            }
        }
    }
    if ok {
        if let Some(k) = group(j) {
            return Some(k);
        }
    }

    // `(?:[0-9a-fA-F]{1,4}:){1,7}:(?:[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4}){0,6})?`
    // The repetition is greedy with backtracking, so the reachable rep counts
    // are collected and tried longest-first.
    let mut ends: Vec<usize> = Vec::new();
    let mut j = i;
    while ends.len() < 7 {
        match group(j) {
            Some(k) if s.get(k) == Some(&':') => {
                j = k + 1;
                ends.push(j);
            }
            _ => break,
        }
    }
    for &after in ends.iter().rev() {
        if s.get(after) != Some(&':') {
            continue;
        }
        let mut k = after + 1;
        // The trailing group list is optional; greedy when present.
        if let Some(mut g) = group(k) {
            k = g;
            for _ in 0..6 {
                if s.get(k) != Some(&':') {
                    break;
                }
                match group(k + 1) {
                    Some(next) => {
                        g = next;
                        k = g;
                    }
                    None => break,
                }
            }
        }
        return Some(k);
    }
    None
}

/// One octet of a dotted quad, as the candidate lengths in the alternation's
/// own preference order (`25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d`).
fn octet_lengths(s: &[char], i: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let d = |j: usize| -> Option<char> { s.get(j).copied().filter(|c| c.is_ascii_digit()) };
    let c0 = match d(i) {
        Some(c) => c,
        None => return out,
    };
    if c0 == '2' {
        if let (Some(c1), Some(c2)) = (d(i + 1), d(i + 2)) {
            // `25[0-5]` then `2[0-4]\d` — two alternatives, one outcome.
            let in_range = (c1 == '5' && ('0'..='5').contains(&c2)) || ('0'..='4').contains(&c1);
            if in_range {
                out.push(3);
            }
        }
    }
    // `1\d\d`
    if c0 == '1' && d(i + 1).is_some() && d(i + 2).is_some() {
        out.push(3);
    }
    // `[1-9]?\d`, greedy: two digits then one.
    if ('1'..='9').contains(&c0) && d(i + 1).is_some() {
        out.push(2);
    }
    out.push(1);
    out.dedup();
    out
}

/// "Octet-range-checked AND fenced by digit boundaries, so a version string like
/// `1.2.3.4000` is not an address." Backtracks across octet lengths exactly as
/// the regex engine would.
fn m_ipv4(s: &[char], i: usize) -> Option<usize> {
    // `(?<![\d.])`
    if i > 0 && (s[i - 1].is_ascii_digit() || s[i - 1] == '.') {
        return None;
    }
    fn walk(s: &[char], at: usize, remaining: usize) -> Option<usize> {
        for len in octet_lengths(s, at) {
            let end = at + len;
            if remaining == 0 {
                // `(?![\d.])`
                let ok = match s.get(end) {
                    None => true,
                    Some(c) => !c.is_ascii_digit() && *c != '.',
                };
                if ok {
                    return Some(end);
                }
                continue;
            }
            if s.get(end) != Some(&'.') {
                continue;
            }
            if let Some(done) = walk(s, end + 1, remaining - 1) {
                return Some(done);
            }
        }
        None
    }
    walk(s, i, 3)
}

/// `[KMGTP]i?B` then `[kmgtp]?[bB]`, in that order.
fn byte_unit(s: &[char], j: usize) -> Option<usize> {
    if let Some(c) = s.get(j) {
        if matches!(c, 'K' | 'M' | 'G' | 'T' | 'P') {
            if s.get(j + 1) == Some(&'i') && s.get(j + 2) == Some(&'B') {
                return Some(j + 3);
            }
            if s.get(j + 1) == Some(&'B') {
                return Some(j + 2);
            }
        }
        if matches!(c, 'k' | 'm' | 'g' | 't' | 'p') && matches!(s.get(j + 1), Some('b') | Some('B'))
        {
            return Some(j + 2);
        }
        if matches!(c, 'b' | 'B') {
            return Some(j + 1);
        }
    }
    None
}

/// "Before `duration`, so the `B` in `5MB` is not read as a bare unit, and
/// before the number kinds."
fn m_bytes(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    let after_int = digits_end(s, i);
    if after_int == i {
        return None;
    }
    // `(?:\.\d+)?` then `\s?`, both greedy with backtracking.
    let mut frac_ends = Vec::new();
    if s.get(after_int) == Some(&'.') {
        let e = digits_end(s, after_int + 1);
        if e > after_int + 1 {
            frac_ends.push(e);
        }
    }
    frac_ends.push(after_int);
    for base in frac_ends {
        let mut spaced = vec![base];
        if matches!(s.get(base), Some(c) if c.is_whitespace()) {
            spaced.insert(0, base + 1);
        }
        for start in spaced {
            if let Some(end) = byte_unit(s, start) {
                if right_fence(s, end) {
                    return Some(end);
                }
            }
        }
    }
    None
}

const DURATION_UNITS: [&str; 8] = ["ns", "µs", "us", "ms", "s", "m", "h", "d"];

fn duration_unit(s: &[char], j: usize) -> Option<usize> {
    for u in DURATION_UNITS {
        let chars: Vec<char> = u.chars().collect();
        if s.len() >= j + chars.len() && s[j..j + chars.len()] == chars[..] {
            return Some(j + chars.len());
        }
    }
    None
}

/// "Before `float`/`int`, which would strand the unit as a word and destroy the
/// template. No space allowed before the unit: `5 m` is far more often `5
/// metres` than five minutes."
fn m_duration(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    let after_int = digits_end(s, i);
    if after_int == i {
        return None;
    }
    let mut bases = Vec::new();
    if s.get(after_int) == Some(&'.') {
        let e = digits_end(s, after_int + 1);
        if e > after_int + 1 {
            bases.push(e);
        }
    }
    bases.push(after_int);
    for base in bases {
        if let Some(end) = duration_unit(s, base) {
            if right_fence(s, end) {
                return Some(end);
            }
        }
    }
    None
}

/// "Bare hex needs 8+ chars and nothing more. Requiring a digit as well — to
/// keep words like `deadbeef` out — turns out to be the wrong trade: an
/// all-letter id such as `bebbccce` is then left unmasked, indexes literally in
/// the clustering tree, and becomes its own singleton pattern."
fn m_hex(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    // `0[xX][0-9a-fA-F]+`
    if s[i] == '0' && matches!(s.get(i + 1), Some('x') | Some('X')) {
        let mut j = i + 2;
        while j < s.len() && is_hex(s[j]) {
            j += 1;
        }
        if j > i + 2 && right_fence(s, j) {
            return Some(j);
        }
    }
    // `[0-9a-fA-F]{8,}` — greedy. Any shorter end is followed by another hex
    // digit, which is a word character, so only the maximal run can clear the
    // right fence; no backtracking is reachable.
    let mut j = i;
    while j < s.len() && is_hex(s[j]) {
        j += 1;
    }
    if j - i >= 8 && right_fence(s, j) {
        return Some(j);
    }
    None
}

/// "Two or more segments, so a lone `/` between words is not a path."
fn m_path(s: &[char], i: usize) -> Option<usize> {
    let seg_char = |c: char| -> bool { is_word(c) || matches!(c, '.' | '@' | '+' | '-') };
    // `(?:~|\.{1,2})?` — present (greedy) before absent.
    let mut prefixes: Vec<usize> = Vec::new();
    if s[i] == '~' {
        prefixes.push(i + 1);
    } else if s[i] == '.' {
        if s.get(i + 1) == Some(&'.') {
            prefixes.push(i + 2);
        }
        prefixes.push(i + 1);
    }
    prefixes.push(i);
    for start in prefixes {
        let mut j = start;
        let mut segments = 0usize;
        loop {
            if s.get(j) != Some(&'/') {
                break;
            }
            let mut k = j + 1;
            while k < s.len() && seg_char(s[k]) {
                k += 1;
            }
            if k == j + 1 {
                break;
            }
            j = k;
            segments += 1;
        }
        if segments >= 2 {
            // `/?`
            if s.get(j) == Some(&'/') {
                j += 1;
            }
            return Some(j);
        }
    }
    None
}

/// "Before `int`, which would take the whole part and leave a stray fraction."
fn m_float(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    let mut j = i;
    if s[j] == '-' {
        j += 1;
    }
    let whole = digits_end(s, j);
    if whole == j || s.get(whole) != Some(&'.') {
        return None;
    }
    let frac = digits_end(s, whole + 1);
    if frac == whole + 1 {
        return None;
    }
    if !right_fence(s, frac) {
        return None;
    }
    Some(frac)
}

/// "Last. Claims only the digits no more specific kind wanted."
fn m_int(s: &[char], i: usize) -> Option<usize> {
    if !left_fence(s, i) {
        return None;
    }
    let mut j = i;
    if s[j] == '-' {
        j += 1;
    }
    let end = digits_end(s, j);
    if end == j || !right_fence(s, end) {
        return None;
    }
    Some(end)
}

type Matcher = fn(&[char], usize) -> Option<usize>;

/// The kinds, in alternation order, with the reason each sits where it does.
/// Rendered by `bough patterns --explain`.
const KINDS: [(VarKind, Matcher, &str); 12] = [
    (VarKind::Quoted, m_quoted, "First, because a quoted string may contain anything — a path, a number, an IP — and those are values of the message, not of the log statement. Matching inside quotes would split one variable into five."),
    (VarKind::Uuid, m_uuid, "Before `hex` and `int`, both of which would claim its first segment and leave the rest as debris."),
    (VarKind::Url, m_url, "Before `path`, `ipv4` and `int`, all of which appear inside a URL. The whole URL is one value."),
    (VarKind::Timestamp, m_timestamp, "A timestamp INSIDE the message (a deadline, an expiry) — the leading one is already gone. Before `int`, which would shred it into six numbers."),
    (VarKind::Ipv6, m_ipv6, "Deliberately conservative: either all eight groups, or a `::` elision. A permissive IPv6 pattern matches `14:22:01` and turns clock times into addresses."),
    (VarKind::Ipv4, m_ipv4, "Octet-range-checked AND fenced by digit boundaries, so a version string like `1.2.3.4000` is not an address — without the trailing fence the pattern happily matches `1.2.3.4` and abandons the `000`. Before `float`, which would take `10.0` and leave `.1.15`."),
    (VarKind::Bytes, m_bytes, "Before `duration`, so the `B` in `5MB` is not read as a bare unit, and before the number kinds."),
    (VarKind::Duration, m_duration, "Before `float`/`int`, which would strand the unit as a word and destroy the template. No space allowed before the unit: `5 m` is far more often `5 metres` than five minutes."),
    (VarKind::Hex, m_hex, "Bare hex needs 8+ chars and nothing more. Requiring a digit as well — to keep words like `deadbeef` out — turns out to be the wrong trade: an all-letter id such as `bebbccce` is then left unmasked, indexes literally in the clustering tree, and becomes its own singleton pattern. On a 500k-line log that produced a hundred junk patterns that should have been one. Length alone is a good enough filter, because English words of 8+ letters drawn only from a-f essentially do not exist, and `deadbeef` genuinely IS a hex constant. Before `int`, which would claim a digit-leading id."),
    (VarKind::Path, m_path, "Two or more segments, so a lone `/` between words is not a path. Before the number kinds, which would carve up `/var/log/app2.log`."),
    (VarKind::Float, m_float, "Before `int`, which would take the whole part and leave a stray fraction."),
    (VarKind::Int, m_int, "Last. Claims only the digits no more specific kind wanted."),
];

// ---------------------------------------------------------------------------
// Magnitudes
// ---------------------------------------------------------------------------

/// Milliseconds per duration unit, for normalizing a slot's quantiles.
fn duration_ms(unit: &str) -> Option<f64> {
    Some(match unit {
        "ns" => 1e-6,
        "µs" | "us" => 1e-3,
        "ms" => 1.0,
        "s" => 1000.0,
        "m" => 60000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        _ => return None,
    })
}

/// Bytes per size unit. Binary and decimal prefixes are both spelled the same
/// way in logs, so both are 1024-based.
fn byte_scale(unit: &str) -> Option<f64> {
    Some(match unit {
        "b" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024f64.powi(2),
        "gb" | "gib" => 1024f64.powi(3),
        "tb" | "tib" => 1024f64.powi(4),
        "pb" | "pib" => 1024f64.powi(5),
        _ => return None,
    })
}

/// Split `raw` into its numeric head and its alphabetic unit tail.
fn split_unit(raw: &str) -> Option<(f64, String)> {
    let chars: Vec<char> = raw.chars().collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_alphabetic() {
        i -= 1;
    }
    if i == chars.len() || i == 0 {
        return None;
    }
    let unit: String = chars[i..].iter().collect();
    let head: String = chars[..i].iter().collect();
    let value: f64 = head.trim().parse().ok()?;
    Some((value, unit))
}

/// The comparable magnitude for a value, in the kind's base unit.
///
/// "Durations become milliseconds and sizes become bytes so that a slot holding
/// both `1.5s` and `900ms` sorts correctly. Quantiles over the bare numerals
/// would rank 900 above 1.5 and report the fast case as the slow one — which is
/// not a rounding error but an inverted answer."
fn magnitude(kind: VarKind, raw: &str) -> Option<f64> {
    match kind {
        VarKind::Int | VarKind::Float => raw.parse::<f64>().ok().filter(|v| v.is_finite()),
        VarKind::Duration => {
            let (v, unit) = split_unit(raw)?;
            Some(v * duration_ms(&unit)?)
        }
        VarKind::Bytes => {
            let (v, unit) = split_unit(raw)?;
            Some(v * byte_scale(&unit.to_lowercase())?)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

/// Mask one line.
///
/// The input should already have had its leading timestamp removed by
/// `strip_timestamp` — this function will happily mask one that is still there,
/// but as a `<timestamp>` variable rather than as the line's clock.
///
/// Offsets (`at`) are CHARACTER offsets into `logtype`, and `stats::tokenize`
/// uses the same unit, which is what keeps attribution aligned.
pub fn mask(line: &str) -> Masked {
    let s: Vec<char> = line.chars().collect();
    let mut values: Vec<VarValue> = Vec::new();
    let mut out = String::new();
    let mut out_chars = 0usize;
    let mut last = 0usize;
    let mut i = 0usize;

    while i < s.len() {
        let mut hit: Option<(VarKind, usize)> = None;
        for (kind, matcher, _) in KINDS {
            if let Some(end) = matcher(&s, i) {
                if end > i {
                    hit = Some((kind, end));
                    break;
                }
            }
        }
        let Some((kind, end)) = hit else {
            i += 1;
            continue;
        };
        let gap: String = s[last..i].iter().collect();
        out.push_str(&gap);
        out_chars += i - last;
        // Recorded BEFORE the placeholder is appended, so `at` points at its `<`.
        let at = out_chars;
        let placeholder = format!("<{}>", kind.as_str());
        out.push_str(&placeholder);
        out_chars += placeholder.chars().count();
        let raw: String = s[i..end].iter().collect();
        let num = magnitude(kind, &raw);
        values.push(VarValue { kind, raw, num, at });
        last = end;
        i = end;
    }
    out.push_str(&s[last..].iter().collect::<String>());
    Masked {
        logtype: out,
        values,
    }
}

/// The kinds and the reason each sits where it does.
pub fn kind_order() -> Vec<(VarKind, &'static str)> {
    KINDS.iter().map(|(k, _, why)| (*k, *why)).collect()
}

#[cfg(test)]
mod tests {
    //! The masking half of `src/logs/mask.test.ts`. "Most of these tests are
    //! order regressions: they pin the alternation order, which is the part
    //! that silently degrades rather than failing loudly when it is wrong."
    use super::*;

    fn kinds(line: &str) -> Vec<&'static str> {
        mask(line).values.iter().map(|v| v.kind.as_str()).collect()
    }

    #[test]
    fn collapses_two_executions_of_one_statement_to_one_logtype() {
        let a = mask("Request from 10.0.1.15 completed in 45ms status=200");
        let b = mask("Request from 10.0.2.99 completed in 1.2s status=404");
        assert_eq!(a.logtype, b.logtype);
        assert_eq!(
            a.logtype,
            "Request from <ipv4> completed in <duration> status=<int>"
        );
    }

    #[test]
    fn keeps_structurally_different_lines_apart() {
        assert_ne!(
            mask("connect to 10.0.1.15").logtype,
            mask("connect to db-primary").logtype
        );
    }

    #[test]
    fn an_ipv4_is_one_value_not_a_float_and_two_ints() {
        assert_eq!(kinds("from 10.0.1.15 ok"), vec!["ipv4"]);
    }

    #[test]
    fn an_address_with_a_port_is_an_address_and_a_port() {
        assert_eq!(
            mask("connect 10.0.1.15:5432 failed").logtype,
            "connect <ipv4>:<int> failed"
        );
    }

    #[test]
    fn a_uuid_survives_whole() {
        assert_eq!(
            kinds("trace 550e8400-e29b-41d4-a716-446655440000 done"),
            vec!["uuid"]
        );
    }

    #[test]
    fn a_url_is_one_value() {
        assert_eq!(
            mask("GET https://api.example.com:8443/v1/users?id=42 -> 200").logtype,
            "GET <url> -> <int>"
        );
    }

    #[test]
    fn a_quoted_string_is_opaque() {
        assert_eq!(
            mask(r#"msg="connect to 10.0.1.15 failed after 3s" code=500"#).logtype,
            "msg=<quoted> code=<int>"
        );
    }

    #[test]
    fn a_size_is_a_size_and_a_duration_is_a_duration() {
        assert_eq!(kinds("wrote 5MB in 250ms"), vec!["bytes", "duration"]);
    }

    #[test]
    fn a_bare_hex_id_needs_eight_characters_and_that_is_the_only_rule() {
        assert_eq!(kinds("session abc123def closed"), vec!["hex"]);
        assert_eq!(kinds("session bebbccce closed"), vec!["hex"]);
        assert!(kinds("the request will accede shortly").is_empty());
    }

    #[test]
    fn a_session_id_is_not_carved_into_fragments() {
        // The word fences: `a107b3f` contains `107b`, a perfectly good byte
        // size, and `c0d9e` contains `0d`, a perfectly good duration.
        assert!(kinds("session a107b3f closed").is_empty());
        assert!(kinds("session c0d9e closed").is_empty());
    }

    #[test]
    fn a_path_is_one_value() {
        assert_eq!(
            mask("reading /var/log/app2.log now").logtype,
            "reading <path> now"
        );
    }

    #[test]
    fn a_lone_slash_between_words_is_not_a_path() {
        assert!(kinds("mode read/write enabled").is_empty());
    }

    #[test]
    fn a_clock_time_is_not_an_ipv6_address() {
        assert_eq!(kinds("elapsed 14:22:01 total"), vec!["int", "int", "int"]);
        assert_eq!(kinds("peer fe80::1 up"), vec!["ipv6"]);
        assert_eq!(
            kinds("peer 2001:0db8:85a3:0000:0000:8a2e:0370:7334 up"),
            vec!["ipv6"]
        );
    }

    #[test]
    fn an_out_of_range_dotted_quad_is_not_an_address() {
        assert_ne!(
            kinds("version 1.2.3.4000 built").first().copied(),
            Some("ipv4")
        );
    }

    #[test]
    fn a_unit_needs_no_space_and_a_word_after_a_number_is_not_a_unit() {
        assert_eq!(kinds("took 5s"), vec!["duration"]);
        assert_eq!(kinds("waited 5 minutes"), vec!["int"]);
        assert_eq!(kinds("scale 5m"), vec!["duration"]);
    }

    #[test]
    fn durations_normalize_to_milliseconds_so_a_slot_can_be_ranked() {
        let n = |s: &str| mask(&format!("took {s}")).values[0].num;
        assert_eq!(n("500ms"), Some(500.0));
        assert_eq!(n("1.5s"), Some(1500.0));
        assert_eq!(n("2m"), Some(120000.0));
        assert_eq!(n("100us"), Some(0.1));
    }

    #[test]
    fn sizes_normalize_to_bytes() {
        let n = |s: &str| mask(&format!("wrote {s}")).values[0].num;
        assert_eq!(n("1KB"), Some(1024.0));
        assert_eq!(n("2MB"), Some(2.0 * 1024.0 * 1024.0));
    }

    #[test]
    fn kinds_without_a_magnitude_carry_none() {
        assert_eq!(mask("from 10.0.1.15").values[0].num, None);
    }

    #[test]
    fn mask_is_stateless_across_calls() {
        let line = "from 10.0.1.15 in 5ms";
        let first = mask(line);
        for _ in 0..5 {
            assert_eq!(mask(line), first);
        }
    }

    #[test]
    fn mask_handles_an_empty_line_and_a_line_with_no_values() {
        assert_eq!(
            mask(""),
            Masked {
                logtype: String::new(),
                values: vec![]
            }
        );
        assert_eq!(
            mask("server started"),
            Masked {
                logtype: "server started".to_string(),
                values: vec![]
            }
        );
    }

    #[test]
    fn placeholder_offsets_point_at_their_own_angle_bracket() {
        let m = mask("from 10.0.1.15 in 5ms");
        assert_eq!(m.logtype, "from <ipv4> in <duration>");
        assert_eq!(m.values[0].at, 5);
        assert_eq!(m.values[1].at, 15);
        assert_eq!(&m.logtype[m.values[0].at..m.values[0].at + 1], "<");
    }

    #[test]
    fn a_mid_line_timestamp_is_one_value() {
        assert_eq!(
            mask("expires 2024-01-15T14:22:01Z soon").logtype,
            "expires <timestamp> soon"
        );
    }

    #[test]
    fn kind_order_is_the_alternation_order() {
        let order: Vec<&str> = kind_order().into_iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "quoted",
                "uuid",
                "url",
                "timestamp",
                "ipv6",
                "ipv4",
                "bytes",
                "duration",
                "hex",
                "path",
                "float",
                "int"
            ]
        );
    }
}
