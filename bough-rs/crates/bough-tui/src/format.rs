//! The TUI's string layer (port of `src/tui/format.ts`, wave-1 subset:
//! styling, markdown-lite, the busy line and the meter line — PORT_PLAN 1.34's
//! "md-basic/busyLine/meterLine". Measurement lives in [`crate::ansi`] and is
//! re-exported here so callers keep the TS module shape).
//!
//! THE INVARIANT THIS HOLDS: **every function here is a pure function of
//! strings and data, correct with no terminal attached.** Nothing reads the
//! terminal, mounts a component, or talks to the server.
//!
//! SECOND INVARIANT — **display width is never a character count.** Every
//! measurement goes through [`crate::ansi::width`] and every slice through the
//! span-based helpers, because these strings carry SGR escapes and OSC 8
//! hyperlinks that occupy zero columns.
//!
//! THIRD — **color is a display setting, not a parameter of the data.** The
//! color flag and the SGR parameter table are module state on purpose: they
//! change how a line is painted and never what it says.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

pub use crate::ansi::{
    ansi_spans, line_from_ansi, slice_ansi, spans_to_line, strip_ansi, truncate_ansi, width,
    wrap_line, AnsiSpan, MIN_WRAP,
};

// ---- color state ------------------------------------------------------------

/// Honor the NO_COLOR convention (<https://no-color.org>) for the hand-rolled
/// SGR paths. Read once; a test flips it with [`set_color_enabled`] rather
/// than mutating the environment.
fn color_cell() -> &'static AtomicBool {
    static COLOR: OnceLock<AtomicBool> = OnceLock::new();
    COLOR.get_or_init(|| {
        AtomicBool::new(std::env::var("NO_COLOR").map(|v| v.is_empty()).unwrap_or(true))
    })
}

pub fn color_enabled() -> bool {
    color_cell().load(Ordering::SeqCst)
}

/// Returns the previous value, so a test can restore it afterwards.
pub fn set_color_enabled(on: bool) -> bool {
    color_cell().swap(on, Ordering::SeqCst)
}

/// SGR *parameter bodies* (what goes between `\x1b[` and `m`), not whole
/// escapes, so a theme can swap 256-color indices for truecolor triples
/// without this file knowing which it got.
#[derive(Clone, Debug)]
pub struct ColorParams {
    pub muted: String,
    pub accent: String,
    pub warn: String,
    pub error: String,
    pub info: String,
    pub code: String,
    pub string: String,
    pub keyword: String,
    pub number: String,
    pub surface_bg: String,
}

impl Default for ColorParams {
    fn default() -> Self {
        ColorParams {
            muted: "38;5;245".into(),
            accent: "38;5;35".into(),
            warn: "38;5;179".into(),
            error: "38;5;167".into(),
            info: "38;5;74".into(),
            code: "38;5;80".into(),
            string: "38;5;107".into(),
            keyword: "38;5;140".into(),
            number: "38;5;179".into(),
            surface_bg: "48;5;236".into(),
        }
    }
}

fn colors_cell() -> &'static RwLock<ColorParams> {
    static COLORS: OnceLock<RwLock<ColorParams>> = OnceLock::new();
    COLORS.get_or_init(|| RwLock::new(ColorParams::default()))
}

/// A snapshot of the live parameter table.
pub fn colors() -> ColorParams {
    colors_cell().read().unwrap().clone()
}

/// How a theme installs itself without editing this module.
pub fn set_colors(update: impl FnOnce(&mut ColorParams)) {
    update(&mut colors_cell().write().unwrap());
}

// ---- styling ----------------------------------------------------------------

fn sgr(params: &str, s: &str, off: &str) -> String {
    if color_enabled() {
        format!("\x1b[{params}m{s}\x1b[{off}m")
    } else {
        s.to_string()
    }
}

/// Foreground spans close with `39m` and bold with `22m`, never a full
/// `\x1b[0m`: a full reset would strip the base color for the rest of the row.
pub fn fg(params: &str, s: &str) -> String {
    sgr(params, s, "39")
}
pub fn bold(s: &str) -> String {
    sgr("1", s, "22")
}
pub fn underline(s: &str) -> String {
    sgr("4", s, "24")
}
/// SGR 3, closed with 23. Terminals without italics ignore the pair.
pub fn italic(s: &str) -> String {
    sgr("3", s, "23")
}
/// SGR 9, closed with 29 — same deal as italics.
pub fn strike(s: &str) -> String {
    sgr("9", s, "29")
}
pub fn dim(s: &str) -> String {
    fg(&colors().muted, s)
}
pub fn accent(s: &str) -> String {
    fg(&colors().accent, s)
}
pub fn warn(s: &str) -> String {
    fg(&colors().warn, s)
}
pub fn danger(s: &str) -> String {
    fg(&colors().error, s)
}
pub fn info(s: &str) -> String {
    fg(&colors().info, s)
}

/// OSC 8 hyperlink. Supporting terminals make the text clickable, the rest
/// ignore the sequence. Zero-width for every measurement in [`crate::ansi`].
pub fn osc8(url: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
    } else {
        text.to_string()
    }
}

// ---- markdown-lite ----------------------------------------------------------
// Terminal styling for prose: headings/bold via SGR bold, `code` spans
// colored, fenced blocks highlighted on a raised surface, "- " bullets
// prettified. Wave-1 "md-basic": the GH-table column layout is a later-wave
// addition — table source reaches the reader as written.

/// A string that is entirely one bare URL (promotes `code`-span URLs to links).
fn is_bare_url(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"));
    match rest {
        Some(rest) => {
            !rest.is_empty()
                && s.chars()
                    .all(|c| !c.is_whitespace() && !matches!(c, ')' | ']' | '>' | '\'' | '"'))
        }
        None => false,
    }
}

fn linkify_url(m: &str) -> String {
    let url = m.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    format!("{}{}", osc8(url, &underline(url)), &m[url.len()..])
}

const JS_WORD: fn(char) -> bool = |c: char| c.is_ascii_alphanumeric() || c == '_';

/// Placeholder-guarded inline pass, exactly the TS order: code spans, bold,
/// strike, `*italic*`, `_italic_`, `[text](url)`, bare URLs, unguard. Guards
/// (`\x00N\x00`) keep a rendered span exempt from later passes — bold still
/// matches across a code span, and a linkified URL is never re-linked.
fn md_inline(line: &str) -> String {
    let mut spans: Vec<String> = Vec::new();
    let s = replace_code_spans(line, &mut spans);
    let s = replace_pair(&s, "**", |inner| !inner.contains('*'), bold);
    let s = replace_pair(&s, "~~", |inner| !inner.contains('~') && !inner.contains('\n'), strike);
    let s = replace_delim_italic(&s, '*', false);
    let s = replace_delim_italic(&s, '_', true);
    let s = replace_md_links(&s, &mut spans);
    let s = replace_bare_urls(&s, &mut spans);
    unguard(&s, &spans)
}

fn guard(spans: &mut Vec<String>, rendered: String) -> String {
    spans.push(rendered);
    format!("\x00{}\x00", spans.len() - 1)
}

/// `` `code` `` — a span that IS a bare URL renders clickable; a URL inside a
/// longer span (a `curl https://…` example) stays literal code.
fn replace_code_spans(s: &str, spans: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find('`') else {
            out.push_str(rest);
            return out;
        };
        let after = &rest[i + 1..];
        match after.find('`') {
            Some(j) if j > 0 => {
                let inner = &after[..j];
                out.push_str(&rest[..i]);
                let rendered = if is_bare_url(inner) {
                    linkify_url(inner)
                } else {
                    fg(&colors().code, inner)
                };
                out.push_str(&guard(spans, rendered));
                rest = &after[j + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 1]);
                rest = &rest[i + 1..];
            }
        }
    }
}

/// `**bold**` / `~~strike~~`: a two-char delimiter pair whose inner run
/// satisfies `ok` and is non-empty.
fn replace_pair(s: &str, delim: &str, ok: impl Fn(&str) -> bool, style: impl Fn(&str) -> String) -> String {
    let stop = delim.chars().next().unwrap();
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find(delim) else {
            out.push_str(rest);
            return out;
        };
        let after = &rest[i + delim.len()..];
        let close = after.find(stop);
        match close {
            Some(p) if p > 0 && after[p..].starts_with(delim) && ok(&after[..p]) => {
                out.push_str(&rest[..i]);
                out.push_str(&style(&after[..p]));
                rest = &after[p + delim.len()..];
            }
            _ => {
                let cut = i + stop.len_utf8();
                out.push_str(&rest[..cut]);
                rest = &rest[cut..];
            }
        }
    }
}

/// `*italic*` and `_italic_`. The delimiter must HUG its text (CommonMark's
/// rule), or `2 * 3 * 4` becomes an italic " 3 ". `_` additionally fires only
/// at word boundaries — `snake_case_name` and `__init__` are identifiers, not
/// emphasis, and they appear in this kind of prose constantly.
fn replace_delim_italic(s: &str, delim: char, word_boundary: bool) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find(delim) else {
            out.push_str(rest);
            return out;
        };
        let after = &rest[i + delim.len_utf8()..];
        let close = after.find(delim);
        let valid = match close {
            Some(p) if p > 0 => {
                let inner = &after[..p];
                let first = inner.chars().next().unwrap();
                let last = inner.chars().last().unwrap();
                let mut ok = !inner.contains('\n')
                    && !first.is_whitespace()
                    && !last.is_whitespace();
                if ok && word_boundary {
                    // (?<![\w\\]) … (?!\w)
                    let prev = rest[..i].chars().last();
                    let next = after[p + delim.len_utf8()..].chars().next();
                    ok = !prev.map(|c| JS_WORD(c) || c == '\\').unwrap_or(false)
                        && !next.map(JS_WORD).unwrap_or(false);
                }
                ok
            }
            _ => false,
        };
        match close {
            Some(p) if valid => {
                out.push_str(&rest[..i]);
                out.push_str(&italic(&after[..p]));
                rest = &after[p + delim.len_utf8()..];
            }
            _ => {
                let cut = i + delim.len_utf8();
                out.push_str(&rest[..cut]);
                rest = &rest[cut..];
            }
        }
    }
}

/// `[text](url)` → clickable underlined text with the url dimmed alongside.
/// A label that IS the url skips the parenthetical — "url (url)" was noise.
/// The `[` of an already-inserted SGR escape is never taken as a link opener.
fn replace_md_links(s: &str, spans: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find('[') else {
            out.push_str(rest);
            return out;
        };
        let escaped = rest[..i].chars().last() == Some('\x1b');
        let parsed = (!escaped)
            .then(|| {
                let after = &rest[i + 1..];
                let j = after.find(']')?;
                if j == 0 {
                    return None;
                }
                let text = &after[..j];
                let tail = &after[j + 1..];
                let tail = tail.strip_prefix('(')?;
                let k = tail.find(')')?;
                let url = &tail[..k];
                if url.is_empty() || url.chars().any(char::is_whitespace) {
                    return None;
                }
                let consumed = i + 1 + j + 2 + k + 1;
                Some((text.to_string(), url.to_string(), consumed))
            })
            .flatten();
        match parsed {
            Some((text, url, consumed)) => {
                out.push_str(&rest[..i]);
                let rendered = if text == url {
                    osc8(&url, &underline(&text))
                } else {
                    osc8(&url, &format!("{} {}", underline(&text), dim(&format!("({url})"))))
                };
                out.push_str(&guard(spans, rendered));
                rest = &rest[consumed..];
            }
            None => {
                out.push_str(&rest[..i + 1]);
                rest = &rest[i + 1..];
            }
        }
    }
}

/// Bare URLs become clickable as themselves; trailing punctuation stays prose.
/// The `\x1b` stop keeps a bolded URL from swallowing its own reset code.
fn replace_bare_urls(s: &str, spans: &mut Vec<String>) -> String {
    let stop = |c: char| {
        c.is_whitespace() || matches!(c, ')' | ']' | '>' | '\'' | '"' | '\x1b')
    };
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find("http") else {
            out.push_str(rest);
            return out;
        };
        let candidate = &rest[i..];
        let scheme_len = if candidate.starts_with("https://") {
            8
        } else if candidate.starts_with("http://") {
            7
        } else {
            out.push_str(&rest[..i + 4]);
            rest = &rest[i + 4..];
            continue;
        };
        let body = &candidate[scheme_len..];
        let end = body.find(stop).unwrap_or(body.len());
        if end == 0 {
            out.push_str(&rest[..i + scheme_len]);
            rest = &rest[i + scheme_len..];
            continue;
        }
        let url = &candidate[..scheme_len + end];
        out.push_str(&rest[..i]);
        out.push_str(&guard(spans, linkify_url(url)));
        rest = &candidate[scheme_len + end..];
    }
}

fn unguard(s: &str, spans: &[String]) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some(i) = rest.find('\x00') else {
            out.push_str(rest);
            return out;
        };
        let after = &rest[i + 1..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let close = after[digits.len()..].starts_with('\x00');
        match (!digits.is_empty() && close)
            .then(|| digits.parse::<usize>().ok())
            .flatten()
            .and_then(|idx| spans.get(idx))
        {
            Some(rendered) => {
                out.push_str(&rest[..i]);
                out.push_str(rendered);
                rest = &after[digits.len() + 1..];
            }
            None => {
                out.push_str(&rest[..i + 1]);
                rest = &rest[i + 1..];
            }
        }
    }
}

// ---- code highlighting ------------------------------------------------------
// A one-pass approximate tokenizer for fenced blocks and program source:
// strings, comments, keywords, numbers. Candy, not a parser — a wrong color on
// an exotic line is fine; a flat gray wall of the program that ran was the bug.

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lang {
    Js,
    Python,
    Go,
    Rust,
    Bash,
    Sql,
}

fn lang_for(tag: &str) -> Lang {
    match tag.to_lowercase().as_str() {
        "python" | "py" => Lang::Python,
        "go" => Lang::Go,
        "rust" | "rs" => Lang::Rust,
        "bash" | "sh" | "zsh" | "shell" => Lang::Bash,
        "sql" => Lang::Sql,
        // js/jsx/ts/tsx/javascript/typescript/json/c/cpp/java and every
        // unknown tag: generic ≈ C-family.
        _ => Lang::Js,
    }
}

fn keywords(lang: Lang) -> &'static str {
    match lang {
        Lang::Js => "const|let|var|function|return|if|else|for|while|do|switch|case|break|continue|new|class|extends|import|export|from|default|try|catch|finally|throw|await|async|typeof|instanceof|in|of|delete|void|yield|static|get|set|this|super|null|undefined|true|false",
        Lang::Python => "def|return|if|elif|else|for|while|break|continue|import|from|as|class|try|except|finally|raise|with|lambda|yield|global|nonlocal|assert|del|pass|and|or|not|in|is|None|True|False|async|await|match|case",
        Lang::Go => "func|return|if|else|for|range|switch|case|break|continue|import|package|type|struct|interface|map|chan|go|defer|select|const|var|nil|true|false",
        Lang::Rust => "fn|return|if|else|for|while|loop|break|continue|use|mod|pub|struct|enum|impl|trait|match|let|mut|const|static|ref|move|async|await|dyn|where|Self|self|None|Some|Ok|Err|true|false",
        Lang::Bash => "if|then|else|elif|fi|for|do|done|while|case|esac|function|return|exit|export|local|readonly|set|unset|shift|source|echo|true|false",
        Lang::Sql => "SELECT|FROM|WHERE|AND|OR|NOT|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|TABLE|INDEX|JOIN|LEFT|RIGHT|INNER|OUTER|ON|AS|ORDER|BY|GROUP|HAVING|LIMIT|NULL|IS|IN|LIKE|BETWEEN|DISTINCT",
    }
}

fn line_comment(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Js | Lang::Go | Lang::Rust => Some("//"),
        Lang::Python | Lang::Bash => Some("#"),
        Lang::Sql => Some("--"),
    }
}

/// One combined regex per language, applied in a single pass so inserted SGR
/// codes are never re-matched (the digits inside an escape would look like a
/// number).
fn hl_regex(lang: Lang) -> &'static regex::Regex {
    static CACHE: OnceLock<std::collections::HashMap<u8, regex::Regex>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        let langs = [Lang::Js, Lang::Python, Lang::Go, Lang::Rust, Lang::Bash, Lang::Sql];
        langs
            .iter()
            .map(|&l| {
                let flags = if l == Lang::Sql { "(?i)" } else { "" };
                let pattern = format!(
                    r#"{flags}("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)|\b({})\b|\b(\d+(?:\.\d+)?)\b"#,
                    keywords(l)
                );
                (l as u8, regex::Regex::new(&pattern).expect("hl regex"))
            })
            .collect()
    });
    cache.get(&(lang as u8)).expect("all langs cached")
}

/// Highlight one line of code. `lang_tag` is the fence tag (`""` is fine).
pub fn highlight_code(line: &str, lang_tag: &str) -> String {
    let lang = lang_for(lang_tag);
    // Split off a trailing line comment first (approximate: marker outside quotes).
    let mut code = line;
    let mut comment = "";
    if let Some(marker) = line_comment(lang) {
        let mut quote: Option<char> = None;
        let mut iter = line.char_indices();
        while let Some((i, c)) = iter.next() {
            if let Some(q) = quote {
                if c == '\\' {
                    iter.next();
                } else if c == q {
                    quote = None;
                }
            } else if c == '"' || c == '\'' || c == '`' {
                quote = Some(c);
            } else if line[i..].starts_with(marker) {
                code = &line[..i];
                comment = &line[i..];
                break;
            }
        }
    }
    let palette = colors();
    let styled = hl_regex(lang).replace_all(code, |caps: &regex::Captures| {
        if let Some(m) = caps.get(1) {
            fg(&palette.string, m.as_str())
        } else if let Some(m) = caps.get(2) {
            fg(&palette.keyword, m.as_str())
        } else {
            fg(&palette.number, &caps[3])
        }
    });
    if comment.is_empty() {
        styled.into_owned()
    } else {
        format!("{styled}{}", dim(comment))
    }
}

/// Paint a subtly raised background behind one rendered line, padded to `w` so
/// a block reads as a contained surface. Any full reset inside the line
/// re-opens the background, so a styled span cannot punch a hole in it.
pub fn surface(line: &str, w: usize) -> String {
    if !color_enabled() {
        return line.to_string();
    }
    let bg = format!("\x1b[{}m", colors().surface_bg);
    let pad = w.saturating_sub(width(line));
    format!(
        "{bg}{}{}\x1b[0m",
        line.replace("\x1b[0m", &format!("\x1b[0m{bg}")),
        " ".repeat(pad)
    )
}

/// Markdown-lite for one block of prose. With `code_width`, fences get a
/// surface.
pub fn md(text: &str, code_width: Option<usize>) -> String {
    let mut fence: Option<String> = None;
    let raise = |line: String| match code_width {
        Some(w) => surface(&line, w),
        None => line,
    };
    text.split('\n')
        .map(|line| {
            // Fence open/close: `^\s*```(\S*)\s*$`.
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("```") {
                let tag: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                if rest[tag.len()..].trim().is_empty() {
                    return match fence.take() {
                        None => {
                            let label = if tag.is_empty() { "code" } else { &tag };
                            let framed = raise(dim(&format!("╭ {label}")));
                            fence = Some(tag);
                            framed
                        }
                        Some(_) => raise(dim("╰")),
                    };
                }
            }
            if let Some(tag) = &fence {
                return raise(format!("{} {}", dim("│"), highlight_code(line, tag)));
            }
            // Headings: `^(#{1,6})\s+(.*)$`.
            let hashes = line.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&hashes) {
                let rest = &line[hashes..];
                if rest.starts_with(char::is_whitespace) {
                    let body = rest.trim_start();
                    return if hashes == 1 {
                        bold(&underline(body))
                    } else {
                        bold(body)
                    };
                }
            }
            // Rule: `^\s*(-{3,}|\*{3,})\s*$`.
            let t = line.trim();
            if t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*')) {
                return dim(&"─".repeat(24));
            }
            // Quote: `^>\s?(.*)$`.
            if let Some(rest) = line.strip_prefix('>') {
                let rest = match rest.chars().next() {
                    Some(c) if c.is_whitespace() => &rest[c.len_utf8()..],
                    _ => rest,
                };
                return dim(&format!("│ {rest}"));
            }
            // Bullet: `^(\s*)- ` → `$1• `.
            let indent: usize = line
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            let bulleted = if line[indent..].starts_with("- ") {
                format!("{}• {}", &line[..indent], &line[indent + 2..])
            } else {
                line.to_string()
            };
            md_inline(&bulleted)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- numbers in view --------------------------------------------------------

/// 1234 → "1.2k", 999 → "999".
pub fn fmt_tokens(n: i64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// 1.234 → "$1.23", 0.0042 → "$0.004" — sub-dollar spend keeps a visible digit.
pub fn fmt_usd(n: f64) -> String {
    if n >= 1.0 {
        format!("${n:.2}")
    } else if n >= 0.001 {
        format!("${n:.3}")
    } else {
        format!("${n:.4}")
    }
}

/// Below this the context chip raises its voice — see [`meter_line`].
pub const CTX_WARN_PCT: i64 = 20;

/// Whole-percent usable context left. `None` when the limit is unknown — an
/// invented percentage is worse than no chip.
pub fn ctx_pct_left(context_tokens: i64, context_limit: Option<i64>) -> Option<i64> {
    let limit = context_limit?;
    if limit <= 0 {
        return None;
    }
    let pct = ((1.0 - context_tokens as f64 / limit as f64) * 100.0).floor() as i64;
    Some(pct.clamp(0, 100))
}

/// Everything the always-visible cost + context line can carry. Pure data in,
/// one string out, so the chat footer is data rather than layout arithmetic.
#[derive(Clone, Debug, Default)]
pub struct MeterOpts {
    pub cost_usd: Option<f64>,
    pub context_tokens: Option<i64>,
    pub context_limit: Option<i64>,
    pub model: Option<String>,
    /// Thinking depth, when it is not the default. It multiplies the price of
    /// every later turn.
    pub effort: Option<String>,
    /// Where the turn runs. Shortened by the caller — this only joins.
    pub workspace: Option<String>,
    /// The branch the workspace is on: "where" is only half an answer.
    pub branch: Option<String>,
    /// Background shells still running. Nothing may run with no pixels on screen.
    pub shells: Option<i64>,
    /// Delegated agents and workflow runs still going — same rule as `shells`.
    pub agents: Option<i64>,
    pub runs: Option<i64>,
    /// Append the `? help` hint. False for surfaces that are not the chat.
    pub help: bool,
    /// This conversation was spawned by another, so `←` goes back to it.
    pub out: bool,
    /// Columns available. `None` = no degradation, the caller accepts any length.
    pub width: Option<usize>,
}

/// The always-visible cost + context line, with its degradation ladder.
///
/// Workspace FIRST and at the bottom of the screen, next to the composer —
/// that is where the eye already is when you are about to press enter. When
/// too narrow, degrade in fixed candidate order rather than wrapping: a status
/// bar that reflows steals a line from the transcript and reads as a rendering
/// bug. Cost and context go last because they are the two numbers that change.
/// What is RUNNING survives degradation as compact glyphs (`⚙2 ◆1 ⧉1`), and
/// `out`/`help` ride down the ladder — the full line always starts with an
/// absolute path, so on a real terminal it ALWAYS degrades, and anything added
/// only to `full` ships invisible.
pub fn meter_line(m: &MeterOpts) -> String {
    let branch = m.branch.as_deref().filter(|b| !b.is_empty());
    let place = |dir: &str| -> String {
        match branch {
            Some(b) if !dir.is_empty() => format!("{dir}@{b}"),
            _ => dir.to_string(),
        }
    };
    let workspace = place(m.workspace.as_deref().unwrap_or(""));
    // The effort rides the model token rather than taking a separator of its
    // own: it is a property OF the model choice, and the two read as one fact.
    let model = match m.model.as_deref().filter(|s| !s.is_empty()) {
        Some(model) => match m.effort.as_deref().filter(|s| !s.is_empty()) {
            Some(effort) => format!("{model} · {effort}"),
            None => model.to_string(),
        },
        None => String::new(),
    };
    let cost = match m.cost_usd {
        Some(c) if c > 0.0 => fmt_usd(c),
        _ => String::new(),
    };
    let context = match m.context_tokens {
        Some(t) if t > 0 => match ctx_pct_left(t, m.context_limit) {
            // bough has no auto-compaction by design, so this chip is the ONLY
            // warning before a turn fails on overflow — and when it warns, it
            // says the way OUT.
            None => format!("{} ctx", fmt_tokens(t)),
            Some(pct) if pct <= CTX_WARN_PCT => format!("⚠ {pct}% ctx left — /compact"),
            Some(pct) => format!("{pct}% ctx left"),
        },
        _ => String::new(),
    };
    let count = |n: Option<i64>, glyph: &str, word: &str| -> String {
        match n {
            Some(n) if n > 0 => {
                format!("{glyph} {n} {word}{}", if n == 1 { "" } else { "s" })
            }
            _ => String::new(),
        }
    };
    let shells = count(m.shells, "⚙", "shell");
    let agents = count(m.agents, "◆", "agent");
    let runs = count(m.runs, "⧉", "run");
    // Glyph-and-number, for the widths where the spelled-out words do not fit.
    // What is running must survive degradation: it is the one part of this row
    // that is a statement about right now.
    let live_bit = |n: Option<i64>, glyph: &str| -> String {
        match n {
            Some(n) if n > 0 => format!("{glyph}{n}"),
            _ => String::new(),
        }
    };
    let live = [
        live_bit(m.shells, "⚙"),
        live_bit(m.agents, "◆"),
        live_bit(m.runs, "⧉"),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    let help = if m.help { "? help" } else { "" }.to_string();
    let out = if m.out { "← back" } else { "" }.to_string();
    let join = |bits: &[&str]| -> String {
        bits.iter()
            .filter(|b| !b.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ")
    };

    let full = join(&[
        &workspace, &model, &cost, &context, &shells, &agents, &runs, &out, &help,
    ]);
    let Some(max) = m.width else { return full };
    if width(&full) <= max {
        return full;
    }

    // Too narrow for everything: the ladder.
    let base = place(
        m.workspace
            .as_deref()
            .unwrap_or("")
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(""),
    );
    let candidates = [
        join(&[&base, &model, &cost, &context, &shells, &agents, &runs, &out, &help]),
        join(&[&model, &cost, &context, &shells, &agents, &runs, &out, &help]),
        join(&[&cost, &context, &live, &out, &help]),
        join(&[&cost, &context, &out, &live]),
        join(&[&context, &live]),
        join(&[&context]),
    ];
    for candidate in candidates {
        if width(&candidate) <= max {
            return candidate;
        }
    }
    truncate_ansi(&full, max, "…")
}

// ---- the busy line ----------------------------------------------------------

/// Braille spinner frames. Ten of them, so the phase reads as motion, not a glitch.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Ticks per spinner cycle, exposed so the caller's interval and this agree.
pub const SPINNER_MS: u64 = 120;

#[derive(Clone, Debug, Default)]
pub struct BusyOpts {
    /// The cheap-tier activity blurb — best-effort by construction; blank or
    /// absent degrades to "working".
    pub activity: Option<String>,
    pub elapsed_ms: i64,
    pub tick: i64,
    /// Tokens streamed SO FAR in this turn. Absent is the normal case for a
    /// provider that reports usage only at the end. Deliberately NOT cost: the
    /// session total on the status row is the number that matters.
    pub tokens: Option<i64>,
}

/// The line shown while a turn is running: motion, elapsed time, and the way
/// out, always. A turn that has printed nothing yet must never look like a
/// frozen terminal.
pub fn busy_line(opts: &BusyOpts) -> String {
    let frame = SPINNER[(opts.tick.unsigned_abs() % SPINNER.len() as u64) as usize];
    let activity = opts.activity.as_deref().unwrap_or("").trim();
    let what = if activity.is_empty() { "working" } else { activity };
    let mut bits: Vec<String> = vec![what.to_string(), fmt_duration(opts.elapsed_ms)];
    if let Some(tokens) = opts.tokens {
        if tokens > 0 {
            bits.push(format!("{} tok", fmt_tokens(tokens)));
        }
    }
    bits.push("esc interrupts".to_string());
    format!("{frame} {}", bits.join(" · "))
}

/// `9s`, `1m04s`. Seconds below a minute; a turn that runs an hour still reads.
pub fn fmt_duration(ms: i64) -> String {
    let total = (ms.div_euclid(1000)).max(0);
    if total < 60 {
        return format!("{total}s");
    }
    let mins = total / 60;
    let secs = total % 60;
    if mins < 60 {
        return format!("{mins}m{secs:02}s");
    }
    format!("{}h{:02}m", mins / 60, mins % 60)
}

// ---- composer completion ----------------------------------------------------

/// Fuzzy rank: exact prefix beats word-boundary prefix beats substring beats
/// in-order subsequence; a non-match scores 0 and drops out.
pub fn fuzzy_score(candidate: &str, query: &str) -> u8 {
    if query.is_empty() {
        return 1;
    }
    let c = candidate.to_lowercase();
    let q = query.to_lowercase();
    if c.starts_with(&q) {
        return 4;
    }
    if ["-", "_", " ", "/"].iter().any(|b| c.contains(&format!("{b}{q}"))) {
        return 3;
    }
    if c.contains(&q) {
        return 2;
    }
    let qc: Vec<char> = q.chars().collect();
    let mut i = 0usize;
    for ch in c.chars() {
        if Some(&ch) == qc.get(i) {
            i += 1;
        }
        if i == qc.len() {
            return 1;
        }
    }
    0
}

/// The candidate indices [`fuzzy_score`] matched, for highlighting a popup row
/// — same tier order, so the marked characters are the ones that made it match.
///
/// CHAR indices, not bytes: they index the label the popup renders glyph by
/// glyph, and every caller (`PopupLabel`) walks it as characters.
pub fn fuzzy_positions(candidate: &str, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let c: Vec<char> = candidate.to_lowercase().chars().collect();
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let run = |start: usize| -> Vec<usize> { (start..start + q.len()).collect() };
    let find = |needle: &[char]| -> Option<usize> {
        if needle.len() > c.len() {
            return None;
        }
        (0..=c.len() - needle.len()).find(|&i| c[i..i + needle.len()] == *needle)
    };
    if c.len() >= q.len() && c[..q.len()] == q[..] {
        return run(0);
    }
    for b in ['-', '_', ' ', '/'] {
        let mut needle = vec![b];
        needle.extend_from_slice(&q);
        if let Some(i) = find(&needle) {
            return run(i + 1);
        }
    }
    if let Some(sub) = find(&q) {
        return run(sub);
    }
    let mut pos: Vec<usize> = Vec::new();
    for (j, ch) in c.iter().enumerate() {
        if pos.len() >= q.len() {
            break;
        }
        if *ch == q[pos.len()] {
            pos.push(j);
        }
    }
    if pos.len() == q.len() {
        pos
    } else {
        Vec::new()
    }
}

/// `file` = an `@` workspace reference; `skill` = a `/` skill invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    File,
    Skill,
}

/// What the composer is currently completing, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// The text between the marker and the cursor — what to rank candidates by.
    pub query: String,
    /// Index of the marker, and the end of the token being replaced. CHAR
    /// indices: the composer's cursor is a char index (keys.rs contract).
    pub start: usize,
    pub end: usize,
}

/// Which completion the cursor is inside.
///
/// THE RULE, and the reason this is a function rather than a `starts_with`
/// check: **both markers fire at ANY word boundary** — position 0 or after
/// whitespace — not only at the start of the input. "look at @src/x.ts"
/// completes a path and "fix this /commit" completes a skill, because a marker
/// mid-input is exactly where a reference belongs in a sentence. The complement
/// matters just as much: a `/` inside a word (`a/path/b`) or an `@` inside one
/// (`user@host`) is NOT a marker and must never swallow the token — that
/// misfire is what makes a picker feel possessed.
///
/// A marker with whitespace between it and the cursor has been left behind: the
/// user finished the reference and moved on, so nothing is being completed.
pub fn active_trigger(text: &str, cursor: usize) -> Option<Trigger> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let end = chars[cursor..]
        .iter()
        .position(|c| c.is_whitespace())
        .map(|i| cursor + i)
        .unwrap_or(chars.len());
    for (marker, kind) in [('/', TriggerKind::Skill), ('@', TriggerKind::File)] {
        // lastIndexOf(marker, cursor - 1): the search starts AT that index.
        let Some(at) = chars[..cursor].iter().rposition(|c| *c == marker) else {
            continue;
        };
        if chars[at + 1..cursor].iter().any(|c| c.is_whitespace()) {
            continue; // the reference is finished
        }
        if at != 0 && !chars[at - 1].is_whitespace() {
            continue; // mid-word: not a marker
        }
        return Some(Trigger {
            kind,
            query: chars[at + 1..cursor].iter().collect(),
            start: at,
            end,
        });
    }
    None
}

/// The directory an `@` query is browsing, when it points OUTSIDE the workspace.
///
/// `git ls-files` is the right candidate source for `@src/x.ts` and cannot
/// answer `@~/notes/todo.md` at all — nothing outside the repo is tracked by it
/// — so a path-shaped query switches the popup to a plain directory listing
/// instead. The shapes that count as "leaving": `~`, an absolute `/`, and
/// explicit `./` or `../`. A bare `src/` is NOT one of them; that is a repo path
/// and stays on git.
///
/// Returns the literal prefix to prepend to each entry — so a completed row
/// reads back as the same path the user was typing — and nothing when the query
/// is a plain workspace reference.
pub fn browse_prefix(query: &str) -> Option<String> {
    let leaves = query.starts_with('~')
        || query.starts_with('/')
        || query.starts_with("./")
        || query.starts_with("../");
    if !leaves {
        return None;
    }
    let q = if query == "~" { "~/".to_string() } else { query.to_string() };
    let cut = q.rfind('/')?;
    Some(q[..cut + 1].to_string())
}

/// A candidate row before ranking: a name, an optional detail, and the built-in
/// command it DISPATCHES (skills and files carry none — they are references).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub detail: String,
    pub run: Option<crate::keys::Command>,
}

impl Candidate {
    pub fn file(name: impl Into<String>) -> Self {
        Self { name: name.into(), detail: String::new(), run: None }
    }
    pub fn skill(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), detail: detail.into(), run: None }
    }
    pub fn command(
        name: impl Into<String>,
        detail: impl Into<String>,
        run: crate::keys::Command,
    ) -> Self {
        Self { name: name.into(), detail: detail.into(), run: Some(run) }
    }
}

/// One popup row. `insert` replaces `[trigger.start, trigger.end)` wholesale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: String,
    pub insert: String,
    /// A built-in `/command` this row DISPATCHES instead of inserting — the
    /// caller still removes `[trigger.start, trigger.end)`, but runs this rather
    /// than leaving `/model` sitting in the draft as text. Absent on skill and
    /// file rows, which are references and belong in the message.
    pub run: Option<crate::keys::Command>,
    /// Label indices the fuzzy match hit, for highlighting.
    pub hl: Vec<usize>,
}

/// `rank_completions`'s answer: the capped rows plus the pre-cap count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranked {
    pub items: Vec<Completion>,
    pub total: usize,
}

/// The popup's row cap. `total` is the pre-cap count so the popup can say
/// "↓ N more" — without it a first-run user reads a six-row menu as the whole
/// catalogue and never types to narrow.
pub const COMPLETION_LIMIT: usize = 6;

/// Rank candidates for a trigger and cap the list.
pub fn rank_completions(candidates: &[Candidate], trigger: &Trigger, limit: usize) -> Ranked {
    let marker = if trigger.kind == TriggerKind::Skill { '/' } else { '@' };
    let mut ranked: Vec<(usize, u8, &Candidate)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (i, fuzzy_score(&c.name, &trigger.query), c))
        .filter(|(_, score, _)| *score > 0)
        .collect();
    // Shorter-is-better is a statement about how WELL a name matched, so it only
    // applies once something was typed. On a bare `/` every candidate scores the
    // same and that tiebreak sorts the menu by name length — which interleaved
    // the built-in commands with whatever skills happen to have short names, at
    // exactly the moment the list is being read as "what can this thing do".
    // With no query, source order wins, and the caller puts the commands first.
    let typed = !trigger.query.is_empty();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| {
                if typed {
                    a.2.name.chars().count().cmp(&b.2.name.chars().count())
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    let total = ranked.len();
    let items = ranked
        .iter()
        .take(limit)
        .map(|(_, _, c)| Completion {
            label: format!("{marker}{}", c.name),
            detail: c.detail.clone(),
            insert: format!(
                "{marker}{}{}",
                c.name,
                if c.name.ends_with('/') { "" } else { " " }
            ),
            run: c.run,
            // Positions are against the bare name; the marker shifts them by one.
            hl: fuzzy_positions(&c.name, &trigger.query).into_iter().map(|p| p + 1).collect(),
        })
        .collect();
    Ranked { items, total }
}

/// Apply a completion to the input, returning the new text and cursor (a CHAR
/// index, like every other cursor in the composer).
pub fn apply_completion(text: &str, trigger: &Trigger, item: &Completion) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let start = trigger.start.min(chars.len());
    let end = trigger.end.min(chars.len()).max(start);
    let head: String = chars[..start].iter().collect();
    let tail: String = chars[end..].iter().collect();
    let insert_len = item.insert.chars().count();
    (format!("{head}{}{tail}", item.insert), start + insert_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Color state is process-global (as it is in TS); tests that flip it
    /// serialize on this lock so parallel test threads cannot race.
    fn color_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_color<T>(on: bool, f: impl FnOnce() -> T) -> T {
        let _guard = color_lock().lock().unwrap();
        let was = set_color_enabled(on);
        let result = f();
        set_color_enabled(was);
        result
    }

    fn link_open(url: &str) -> String {
        format!("\x1b]8;;{url}\x1b\\")
    }
    const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

    #[test]
    fn no_color_is_honored_and_every_styled_helper_degrades_to_plain_text() {
        with_color(false, || {
            assert!(!color_enabled());
            assert_eq!(md("**bold** and `code`", None), "bold and code");
            assert_eq!(surface("hi", 10), "hi"); // no background, no padding
        });
    }

    // ---- markdown-lite -------------------------------------------------------

    #[test]
    fn md_markdown_links_become_one_osc8_hyperlink_not_two() {
        with_color(true, || {
            let out = md("see [the docs](https://example.com/x)", None);
            assert!(out.contains(&link_open("https://example.com/x")), "{out:?}");
            assert!(out.contains(LINK_CLOSE));
            // The dimmed (url) is not re-linked.
            assert_eq!(out.matches("]8;;").count(), 2, "{out:?}");
        });
    }

    #[test]
    fn md_a_code_span_that_is_a_url_is_clickable_one_inside_a_command_stays_literal() {
        with_color(true, || {
            let url = "http://localhost:4321/artifacts/s1/x.html";
            assert!(md(&format!("`{url}`"), None).contains(&link_open(url)));
            assert!(md(&format!("**{url}**"), None).contains(&link_open(url)));
            assert!(!md("run `curl https://example.com`", None).contains("]8;;"));
        });
    }

    #[test]
    fn md_fenced_code_sits_on_a_raised_surface_when_a_width_is_given() {
        with_color(true, || {
            let out = md("```js\nconst x = 1\n```", Some(40));
            assert!(out.contains("\x1b[48;"), "the block needs a background: {out:?}");
            assert!(!md("plain prose", Some(40)).contains("\x1b[48;"));
            let line = surface("hi", 10);
            assert!(line.ends_with(&format!("{}\x1b[0m", " ".repeat(8))), "{line:?}");
        });
    }

    #[test]
    fn md_emphasis_renders_and_identifiers_with_underscores_are_left_alone() {
        with_color(false, || {
            // Bold already worked; `*italic*` and `_italic_` reached the screen
            // as literal asterisks and underscores in the middle of prose.
            assert_eq!(md("Some *emphasis* here", None), "Some emphasis here");
            assert_eq!(md("Some _emphasis_ here", None), "Some emphasis here");
            assert_eq!(md("Some **strong** here", None), "Some strong here");
            // Bold is matched FIRST, so `**x**` is never seen as two single-star spans.
            assert_eq!(md("**a** and *b*", None), "a and b");
            // Identifiers are not emphasis, and this kind of prose is full of them.
            assert_eq!(
                md("call snake_case_name and __init__ now", None),
                "call snake_case_name and __init__ now"
            );
            // A lone star in prose (a footnote marker, a glob) stays put.
            assert_eq!(md("use *.ts files", None), "use *.ts files");
            // The delimiter must hug its text: `2 * 3 * 4` is a product, not an
            // italic " 3 ".
            assert_eq!(md("2 * 3 * 4 = 24", None), "2 * 3 * 4 = 24");
            assert_eq!(md("a_b _c_ d", None), "a_b c d");
            // Strikethrough, which a model uses for a superseded step.
            assert_eq!(md("~~gone~~ and kept", None), "gone and kept");
        });
    }

    #[test]
    fn md_a_lone_pipe_is_prose_and_a_table_inside_a_fence_is_code() {
        with_color(false, || {
            // No table layout in md-basic — but the invariants hold either way:
            // a shell pipe in a sentence survives…
            let prose = md("run `wc -l < a.txt | tr -d ' '` to count", None);
            assert!(prose.contains('|'), "{prose:?}");
            // …and fenced source reaches the reader as written.
            let fenced = "```markdown\n| a | b |\n|---|---|\n| 1 | 2 |\n```";
            let out = md(fenced, None);
            assert!(out.contains("| a | b |"), "{out:?}");
            assert!(out.contains("|---|---|"), "{out:?}");
        });
    }

    #[test]
    fn md_block_furniture_headings_rules_quotes_bullets() {
        with_color(false, || {
            assert_eq!(md("# Title", None), "Title");
            assert_eq!(md("## Sub", None), "Sub");
            assert_eq!(md("---", None), "─".repeat(24));
            assert_eq!(md("> quoted", None), "│ quoted");
            assert_eq!(md("- item", None), "• item");
            assert_eq!(md("  - nested", None), "  • nested");
            // A fence with no tag frames as "code".
            assert_eq!(md("```\nx\n```", None), "╭ code\n│ x\n╰");
        });
    }

    #[test]
    fn highlight_code_colors_strings_keywords_numbers_and_splits_comments() {
        with_color(true, || {
            let out = highlight_code("const x = \"hi\" // note", "js");
            let p = colors();
            assert!(out.contains(&format!("\x1b[{}mconst\x1b[39m", p.keyword)), "{out:?}");
            assert!(out.contains(&format!("\x1b[{}m\"hi\"\x1b[39m", p.string)), "{out:?}");
            assert!(out.contains(&format!("\x1b[{}m// note\x1b[39m", p.muted)), "{out:?}");
            // A marker inside a string is not a comment.
            let quoted = highlight_code("const u = \"http://x\"", "js");
            assert!(!quoted.contains(&format!("\x1b[{}m", p.muted)), "{quoted:?}");
        });
    }

    // ---- measurement, through the md integration -----------------------------

    #[test]
    fn wrap_and_truncate_measure_md_output_by_display_width() {
        with_color(true, || {
            let styled = md("**alphabet** soup with rather a lot of words in it", None);
            assert!(styled.len() > 49); // escapes inflate the character count…
            for l in wrap_line(&styled, 20) {
                assert!(width(&l) <= 20, "row measured {}: {l:?}", width(&l));
            }
            let cut = truncate_ansi(&md("**abcdefghij** klmno", None), 6, "");
            assert_eq!(width(&cut), 6);
            assert!(cut.contains("\x1b["), "escapes must survive the slice");
            let code = md("`abcdef`", None);
            assert_eq!(width(&code), 6);
            assert_eq!(truncate_ansi(&code, 10, ""), code); // fits: untouched
            assert_eq!(width(&truncate_ansi(&code, 3, "")), 3);
        });
    }

    // ---- numbers -------------------------------------------------------------

    #[test]
    fn fmt_tokens_usd_and_ctx_pct_left() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(184_000), "184k");
        assert_eq!(fmt_usd(1.234), "$1.23");
        assert_eq!(fmt_usd(0.0042), "$0.004");
        assert_eq!(fmt_usd(0.00004), "$0.0000");
        assert_eq!(ctx_pct_left(50_000, Some(200_000)), Some(75));
        assert_eq!(ctx_pct_left(300_000, Some(200_000)), Some(0));
        assert_eq!(ctx_pct_left(10, None), None);
    }

    // ---- the meter -----------------------------------------------------------

    #[test]
    fn meter_an_unknown_context_limit_shows_tokens_never_a_made_up_percent() {
        assert_eq!(
            meter_line(&MeterOpts {
                model: Some("opus".into()),
                cost_usd: Some(1.5),
                context_tokens: Some(50_000),
                context_limit: Some(200_000),
                ..Default::default()
            }),
            "opus · $1.50 · 75% ctx left"
        );
        assert_eq!(
            meter_line(&MeterOpts {
                model: Some("opus".into()),
                context_tokens: Some(50_000),
                ..Default::default()
            }),
            "opus · 50k ctx"
        );
        assert_eq!(meter_line(&MeterOpts::default()), "");
    }

    #[test]
    fn the_meter_carries_the_whole_session_status_in_one_line_at_the_bottom() {
        assert_eq!(
            meter_line(&MeterOpts {
                workspace: Some("~/repos/x".into()),
                model: Some("claude-opus-5".into()),
                cost_usd: Some(0.0072),
                context_tokens: Some(18_000),
                context_limit: Some(200_000),
                help: true,
                ..Default::default()
            }),
            "~/repos/x · claude-opus-5 · $0.007 · 91% ctx left · ? help"
        );
        // A fresh conversation has no model, no spend and no context — and must
        // still say where it will run and how to get help.
        assert_eq!(
            meter_line(&MeterOpts {
                workspace: Some("~/repos/x".into()),
                help: true,
                ..Default::default()
            }),
            "~/repos/x · ? help"
        );
        // An unpriced model has no window in the catalog, so the raw count
        // stands in rather than a fabricated percentage.
        assert_eq!(
            meter_line(&MeterOpts {
                model: Some("who/knows".into()),
                context_tokens: Some(18_000),
                context_limit: None,
                ..Default::default()
            }),
            "who/knows · 18k ctx"
        );
    }

    #[test]
    fn a_narrow_terminal_degrades_the_meter_instead_of_wrapping_it() {
        let m = MeterOpts {
            workspace: Some("~/repos/bough".into()),
            model: Some("moonshotai/kimi-k3".into()),
            cost_usd: Some(0.007),
            context_tokens: Some(18_000),
            context_limit: Some(1_048_576),
            help: true,
            ..Default::default()
        };
        assert_eq!(
            meter_line(&MeterOpts { width: Some(200), ..m.clone() }),
            "~/repos/bough · moonshotai/kimi-k3 · $0.007 · 98% ctx left · ? help"
        );
        // Every degraded form still fits, and the two live numbers survive longest.
        for w in [70usize, 60, 50, 40, 30, 20, 12] {
            let line = meter_line(&MeterOpts { width: Some(w), ..m.clone() });
            assert!(width(&line) <= w, "width {w} produced {} cols: {line}", width(&line));
        }
        // The workspace shortens to its basename before it disappears entirely.
        assert!(meter_line(&MeterOpts { width: Some(60), ..m.clone() }).contains("bough"));
        // At the narrowest, context left is the last thing standing.
        assert_eq!(
            meter_line(&MeterOpts { width: Some(14), ..m.clone() }),
            "98% ctx left"
        );

        // What is RUNNING survives degradation.
        let live = MeterOpts {
            shells: Some(2),
            agents: Some(3),
            runs: Some(1),
            ..m.clone()
        };
        assert!(meter_line(&live).contains("⚙ 2 shells · ◆ 3 agents · ⧉ 1 run"));
        assert!(meter_line(&MeterOpts { agents: Some(1), runs: Some(2), ..live.clone() })
            .contains("◆ 1 agent · ⧉ 2 runs"));
        // Narrow: the words collapse to glyphs rather than dropping the fact.
        let narrow = meter_line(&MeterOpts { width: Some(30), ..live.clone() });
        assert!(narrow.contains("⚙2 ◆3 ⧉1"), "{narrow}");
        assert!(width(&narrow) <= 30, "{narrow}");
        // Nothing running says nothing at all.
        assert_eq!(
            meter_line(&MeterOpts {
                shells: Some(0),
                agents: Some(0),
                runs: Some(0),
                ..m.clone()
            }),
            meter_line(&m)
        );

        // The warning names the way out — this chip is the only notice before a
        // turn fails on overflow.
        let tight = meter_line(&MeterOpts {
            context_tokens: Some(195_000),
            context_limit: Some(200_000),
            ..Default::default()
        });
        assert!(tight.starts_with("⚠ "), "{tight}");
        assert!(tight.ends_with("% ctx left — /compact"), "{tight}");
        // Above the threshold it stays a plain number.
        assert_eq!(
            meter_line(&MeterOpts {
                context_tokens: Some(20_000),
                context_limit: Some(200_000),
                ..Default::default()
            }),
            "90% ctx left"
        );
    }

    #[test]
    fn a_spawned_conversation_says_how_to_get_back_to_the_one_that_spawned_it() {
        let base = MeterOpts {
            workspace: Some("/w/repo".into()),
            model: Some("claude-haiku-4-5".into()),
            help: true,
            ..Default::default()
        };
        assert!(meter_line(&MeterOpts { out: true, ..base.clone() }).contains("← back"));
        // A root conversation has nowhere to go back to, and must not offer.
        assert!(!meter_line(&base).contains("← back"));
        // It sits before the help hint, which stays last.
        let line = meter_line(&MeterOpts { out: true, ..base.clone() });
        assert!(line.find("← back").unwrap() < line.find("? help").unwrap(), "{line}");
        // AND IT SURVIVES DEGRADATION: the full line begins with an ABSOLUTE
        // workspace path, so on a real terminal it always degrades — a chip
        // added only to the full form is a chip that never renders.
        let narrow = meter_line(&MeterOpts {
            workspace: Some("/private/tmp/a/very/long/path/that/forces/the/ladder/sa-bough".into()),
            branch: Some("main".into()),
            model: Some("claude-haiku-4-5".into()),
            cost_usd: Some(0.003),
            context_tokens: Some(14_000),
            context_limit: Some(200_000),
            help: true,
            out: true,
            width: Some(100),
            ..Default::default()
        });
        assert!(narrow.contains("← back"), "{narrow}");
        assert!(width(&narrow) <= 100, "{}: {narrow}", width(&narrow));
    }

    // ---- the busy line -------------------------------------------------------

    #[test]
    fn the_busy_line_always_names_motion_elapsed_time_and_the_way_out() {
        // The regression it prevents: a running turn that has printed nothing
        // looked identical to a hung terminal, and esc — the fix for a hung
        // terminal — was documented on no screen.
        let line = busy_line(&BusyOpts { activity: None, elapsed_ms: 9_000, tick: 0, tokens: None });
        assert_eq!(line, "⠋ working · 9s · esc interrupts");
        // The cheap-tier blurb rides along when there is one, instead of replacing it.
        assert_eq!(
            busy_line(&BusyOpts {
                activity: Some("reading keys.ts".into()),
                elapsed_ms: 0,
                tick: 1,
                tokens: None
            }),
            "⠙ reading keys.ts · 0s · esc interrupts"
        );
        // A blank blurb is the same as no blurb — never an empty middle field.
        assert_eq!(
            busy_line(&BusyOpts { activity: Some("   ".into()), elapsed_ms: 0, tick: 0, tokens: None }),
            "⠋ working · 0s · esc interrupts"
        );
        // The spinner cycles rather than running off the end of the frame list.
        let frames: std::collections::HashSet<String> = (0..40)
            .map(|i| {
                busy_line(&BusyOpts { activity: None, elapsed_ms: 0, tick: i, tokens: None })
                    .chars()
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(frames.len(), 10);
    }

    #[test]
    fn the_busy_line_carries_the_turns_own_tokens_while_it_runs_but_not_its_cost() {
        assert_eq!(
            busy_line(&BusyOpts {
                activity: None,
                elapsed_ms: 42_000,
                tick: 0,
                tokens: Some(3_200)
            }),
            "⠋ working · 42s · 3.2k tok · esc interrupts"
        );
        // A provider that reports usage only at the end leaves zeros here, and a
        // zero is omitted rather than printed.
        assert_eq!(
            busy_line(&BusyOpts { activity: None, elapsed_ms: 1_000, tick: 0, tokens: Some(0) }),
            "⠋ working · 1s · esc interrupts"
        );
    }

    #[test]
    fn fmt_duration_stays_readable_from_one_second_to_hours() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(9_400), "9s");
        assert_eq!(fmt_duration(59_999), "59s");
        assert_eq!(fmt_duration(64_000), "1m04s");
        assert_eq!(fmt_duration(3_600_000), "1h00m");
        assert_eq!(fmt_duration(-5), "0s");
    }

    // ---- composer completion (format.test.ts) ------------------------------

    fn trig(text: &str, cursor: usize) -> Trigger {
        active_trigger(text, cursor).expect("a trigger under the cursor")
    }

    #[test]
    fn fuzzy_score_prefix_beats_boundary_beats_substring_beats_subsequence() {
        assert_eq!(fuzzy_score("exa", "ex"), 4);
        assert_eq!(fuzzy_score("user-testing", "test"), 3);
        assert_eq!(fuzzy_score("src/server/app.ts", "server"), 3); // "/" is a boundary too
        assert_eq!(fuzzy_score("restish", "tish"), 2);
        assert_eq!(fuzzy_score("wiki", "wk"), 1);
        assert_eq!(fuzzy_score("commit", "xyz"), 0);
        assert_eq!(fuzzy_score("anything", ""), 1);
    }

    #[test]
    fn fuzzy_positions_marks_the_characters_that_made_it_match() {
        assert_eq!(fuzzy_positions("exa", "ex"), vec![0, 1]);
        assert_eq!(fuzzy_positions("user-testing", "test"), vec![5, 6, 7, 8]);
        assert_eq!(fuzzy_positions("restish", "tish"), vec![3, 4, 5, 6]);
        assert_eq!(fuzzy_positions("wiki", "wk"), vec![0, 2]);
        assert!(fuzzy_positions("commit", "xyz").is_empty());
        assert!(fuzzy_positions("anything", "").is_empty());
    }

    #[test]
    fn active_trigger_fires_at_any_word_boundary_not_just_position_zero() {
        assert_eq!(
            active_trigger("@src", 4),
            Some(Trigger { kind: TriggerKind::File, query: "src".into(), start: 0, end: 4 })
        );
        assert_eq!(
            active_trigger("look at @src", 12),
            Some(Trigger { kind: TriggerKind::File, query: "src".into(), start: 8, end: 12 })
        );
        assert_eq!(
            active_trigger("/com", 4),
            Some(Trigger { kind: TriggerKind::Skill, query: "com".into(), start: 0, end: 4 })
        );
        assert_eq!(
            active_trigger("fix this /com", 13),
            Some(Trigger { kind: TriggerKind::Skill, query: "com".into(), start: 9, end: 13 })
        );
        // A bare marker completes everything — the menu opens on the marker alone.
        assert_eq!(
            active_trigger("@", 1),
            Some(Trigger { kind: TriggerKind::File, query: String::new(), start: 0, end: 1 })
        );
    }

    #[test]
    fn active_trigger_a_marker_mid_word_is_not_a_marker() {
        assert_eq!(active_trigger("src/server/app", 14), None); // a path, not a skill
        assert_eq!(active_trigger("user@host", 9), None); // an address, not a reference
        // …but a real one still fires.
        assert_eq!(active_trigger("a/b @c/d", 8).map(|t| t.kind), Some(TriggerKind::File));
    }

    #[test]
    fn active_trigger_a_finished_reference_stops_completing() {
        assert_eq!(active_trigger("@src/x.ts now what", 18), None);
        assert_eq!(active_trigger("plain text", 10), None);
        assert_eq!(active_trigger("", 0), None);
    }

    #[test]
    fn active_trigger_replaces_the_token_under_the_cursor_whole() {
        // Cursor sits mid-token; `end` runs to the next whitespace so accepting a
        // completion cannot leave the tail of the old word behind.
        let t = trig("@ser/app.ts tail", 4);
        assert_eq!(t.query, "ser");
        assert_eq!(t.end, 11);
    }

    #[test]
    fn browse_prefix_only_a_path_that_leaves_the_workspace_browses_the_filesystem() {
        assert_eq!(browse_prefix("~/repos/bo").as_deref(), Some("~/repos/"));
        assert_eq!(browse_prefix("~").as_deref(), Some("~/")); // a bare `@~` opens home
        assert_eq!(browse_prefix("/etc/ho").as_deref(), Some("/etc/"));
        assert_eq!(browse_prefix("./sr").as_deref(), Some("./"));
        assert_eq!(browse_prefix("../sibling/x").as_deref(), Some("../sibling/"));
        // A plain repo path stays on `git ls-files` — that is where its candidates are.
        assert_eq!(browse_prefix("src/tui/"), None);
        assert_eq!(browse_prefix(""), None);
    }

    #[test]
    fn browse_prefix_entries_rank_as_the_full_path_the_user_is_typing() {
        let trigger = trig("@~/rep", 6);
        let prefix = browse_prefix(&trigger.query).unwrap();
        let candidates: Vec<Candidate> = ["repos/", "Desktop/", ".zshrc"]
            .iter()
            .map(|name| Candidate::file(format!("{prefix}{name}")))
            .collect();
        let Ranked { items, .. } = rank_completions(&candidates, &trigger, COMPLETION_LIMIT);
        assert_eq!(items[0].label, "@~/repos/");
        // A directory keeps its slash and gains no trailing space, so accepting it
        // re-triggers and drills one level down instead of ending the reference.
        assert_eq!(items[0].insert, "@~/repos/");
    }

    #[test]
    fn rank_completions_replaces_the_token_and_reports_what_was_hidden() {
        let trigger = trig("look at @app", 12);
        let files: Vec<Candidate> = [
            "server/app.ts",
            "app.tsx",
            "components/Chat.tsx",
            "apparatus/x.ts",
            "a/p/p.ts",
            "docs/app.md",
            "old/app.js",
            "zap/app.rs",
        ]
        .iter()
        .map(|n| Candidate::file(*n))
        .collect();
        let Ranked { items, total } = rank_completions(&files, &trigger, 3);
        assert_eq!(items.len(), 3);
        assert_eq!(total, 7); // "components/Chat.tsx" does not match at all
        assert_eq!(items[0].label, "@app.tsx"); // exact prefix wins
        assert!(!items[0].hl.is_empty());
        let applied = apply_completion("look at @app", &trigger, &items[0]);
        assert_eq!(applied, ("look at @app.tsx ".to_string(), 17));
    }

    #[test]
    fn rank_completions_a_directory_candidate_inserts_without_a_trailing_space() {
        let trigger = trig("@sr", 3);
        let items = rank_completions(&[Candidate::file("src/")], &trigger, COMPLETION_LIMIT).items;
        assert_eq!(items[0].insert, "@src/"); // keep typing into the directory
    }

    #[test]
    fn rank_completions_a_skill_trigger_marks_rows_with_the_slash_it_will_insert() {
        let trigger = trig("/his", 4);
        let items = rank_completions(
            &[Candidate::skill("history", "query bough's SQLite")],
            &trigger,
            COMPLETION_LIMIT,
        )
        .items;
        assert_eq!(items[0].label, "/history");
        assert_eq!(items[0].insert, "/history ");
        assert_eq!(items[0].detail, "query bough's SQLite");
    }

    #[test]
    fn rank_completions_with_no_query_keeps_source_order_so_the_built_ins_lead() {
        // App.tsx puts the commands first; the length tiebreak must not
        // interleave short skill names into them on a bare `/`.
        let trigger = trig("/", 1);
        let candidates = vec![
            Candidate::command("model", "pick the model", crate::keys::Command::HelpOpen),
            Candidate::skill("go", "a two-letter skill"),
        ];
        let items = rank_completions(&candidates, &trigger, COMPLETION_LIMIT).items;
        assert_eq!(items[0].label, "/model");
        assert_eq!(items[0].run, Some(crate::keys::Command::HelpOpen));
        assert_eq!(items[1].label, "/go");
        assert_eq!(items[1].run, None);
    }
}
