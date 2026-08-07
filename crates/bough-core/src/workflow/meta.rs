//! `export const meta = {…}` — read out of a workflow script WITHOUT running
//! it (port of `src/workflow/meta.ts`).
//!
//! A run's name, description and phase list have to be known before the script
//! executes: they are what the run row is created with, what the run view
//! labels its phases from, and what a rerun inherits when the author edited
//! only the body. The script itself runs detached, minutes to hours later, in
//! the workflow worker — so "just run it and read the export" is not available
//! at submit time.
//!
//! THE INVARIANT THIS HOLDS: **`meta` is a pure literal, located by a scan that
//! cannot be derailed by the script's own text, and evaluated by a parser that
//! cannot execute anything.** Two halves, both load-bearing:
//!
//!   1. *Finding it* is a balanced-brace scan that skips string bodies,
//!      template bodies (including nested `${…}` interpolations, which contain
//!      braces), line comments and block comments. The naive `indexOf("}")` —
//!      or a regex — stops at the first brace inside
//!      `description: "handles {a} and {b}"`. So does a
//!      `// TODO: export const meta = {` above the real declaration, which is
//!      why the DECLARATION is located by the same skipping scan.
//!   2. *Reading it* is a recursive-descent parser over object/array/string/
//!      number/boolean/null literals and nothing else. `name: NAME`,
//!      `description: head + tail`, `` `audit ${target}` ``,
//!      `phases: phasesFor(x)` and `{ ...defaults }` are all REJECTED, each
//!      with a message saying why — the host never runs the script, so a
//!      computed value is not a thing it can resolve.
//!
//! Pure, synchronous, no worker, no clock, no filesystem: the whole module is
//! string math over a submitted script, which is what lets the submit boundary
//! reject a bad script in the same request that posted it.
//!
//! Offsets are CHAR offsets throughout (the TS original counts UTF-16 units);
//! they are internal to this module and to [`strip_meta`], which rebuilds the
//! script from the same units, so the two never meet a byte index.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::errors::BoughError;
use crate::schema::parts::WorkflowPhase;

// ---------------------------------------------------------------------------
// The validated shape
// ---------------------------------------------------------------------------

/// What a script must declare. Unknown keys are REJECTED (the TS `.strict()`)
/// because a silently dropped key is the worst outcome here: `phasez: [...]`
/// stripped as unknown produces a run with no phases and no complaint, and the
/// author debugs the run view instead of the typo.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<WorkflowPhase>>,
}

/// Where the `export const meta = {…}` statement sits in the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaSpan {
    /// Offset of `export`.
    pub start: usize,
    /// Offset of the opening `{`.
    pub literal_start: usize,
    /// Offset just past the closing `}`.
    pub end: usize,
    /// The literal text, `{` through `}` inclusive.
    pub literal: String,
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// 1-based line of an offset — every message points at a line the author can see.
fn line_of(src: &[char], index: usize) -> usize {
    let mut line = 1;
    for (i, c) in src.iter().enumerate() {
        if i >= index {
            break;
        }
        if *c == '\n' {
            line += 1;
        }
    }
    line
}

/// The one message for every computed value, because the author's next move is
/// the same in every case and the *reason* is the part they are missing: meta
/// is read, never executed.
fn computed(what: &str, src: &[char], at: usize) -> BoughError {
    BoughError::workflow_script(format!(
        "workflow meta must be a pure literal — {what} on line {} is computed. The host reads \
         `meta` by scanning the source and never runs the script, so it cannot resolve \
         variables, calls, operators, spreads or `${{…}}` interpolation. Write the value out \
         literally (name: 'audit-handlers'), and compute whatever is dynamic inside the script \
         body, where it runs.",
        line_of(src, at)
    ))
}

fn malformed(what: &str, src: &[char], at: usize) -> BoughError {
    BoughError::workflow_script(format!(
        "workflow meta does not parse: {what} on line {}. `meta` must be a literal object: \
         {{name, description, phases?: [{{title, detail?}}]}}.",
        line_of(src, at)
    ))
}

// ---------------------------------------------------------------------------
// The scan (half 1: find it, undeceived by the script's own text)
// ---------------------------------------------------------------------------

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Consume a `'`/`"` string starting at its opening quote. Returns the offset
/// just past the closing quote, or `None` when it is unterminated — including
/// by a raw newline, which in JS closes nothing and is the single most common
/// way a hand-written script's brace balance goes wrong.
fn skip_quoted(src: &[char], at: usize) -> Option<usize> {
    let quote = src[at];
    let mut i = at + 1;
    while i < src.len() {
        let c = src[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '\n' {
            return None;
        }
        if c == quote {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Frames of the scan. A template body is its own mode — braces inside it are
/// text — and each `${…}` opens a fresh CODE frame with its own brace depth, so
/// the `}` that closes an interpolation is never mistaken for the `}` that
/// closes `meta`.
enum Frame {
    Code { depth: i64 },
    Template,
}

/// From the opening `{` at `start`, return the offset just past its matching
/// `}`. Errors when the literal never closes — a truncated paste, or a string
/// closed by a newline — rather than returning a silently short literal.
pub fn scan_balanced(src: &str, start: usize) -> Result<usize, BoughError> {
    scan_balanced_chars(&src.chars().collect::<Vec<char>>(), start)
}

fn scan_balanced_chars(src: &[char], start: usize) -> Result<usize, BoughError> {
    let mut stack: Vec<Frame> = vec![Frame::Code { depth: 0 }];
    let mut i = start;
    while i < src.len() {
        let c = src[i];
        let top = stack.last_mut().expect("stack is never empty");

        if let Frame::Template = top {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '`' {
                stack.pop();
                i += 1;
                continue;
            }
            if c == '$' && src.get(i + 1) == Some(&'{') {
                stack.push(Frame::Code { depth: 0 });
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if c == '"' || c == '\'' {
            match skip_quoted(src, i) {
                None => {
                    let kind = if c == '"' { "double" } else { "single" };
                    return Err(malformed(
                        &format!("a {kind}-quoted string is never closed"),
                        src,
                        i,
                    ));
                }
                Some(next) => {
                    i = next;
                    continue;
                }
            }
        }
        if c == '`' {
            stack.push(Frame::Template);
            i += 1;
            continue;
        }
        if c == '/' && src.get(i + 1) == Some(&'/') {
            match find_char(src, i, '\n') {
                None => break, // comment runs to EOF: the literal never closes
                Some(nl) => {
                    i = nl + 1;
                    continue;
                }
            }
        }
        if c == '/' && src.get(i + 1) == Some(&'*') {
            match find_seq(src, i + 2, &['*', '/']) {
                None => return Err(malformed("a block comment is never closed", src, i)),
                Some(end) => {
                    i = end + 2;
                    continue;
                }
            }
        }
        if c == '{' {
            if let Frame::Code { depth } = top {
                *depth += 1;
            }
            i += 1;
            continue;
        }
        if c == '}' {
            if let Frame::Code { depth } = top {
                if *depth == 0 {
                    // Closes the `${…}` this frame was opened by.
                    if stack.len() > 1 {
                        stack.pop();
                        i += 1;
                        continue;
                    }
                    return Err(malformed("an unbalanced `}`", src, i));
                }
                *depth -= 1;
                if *depth == 0 && stack.len() == 1 {
                    return Ok(i + 1);
                }
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    Err(malformed("the `meta` literal is never closed", src, start))
}

fn find_char(src: &[char], from: usize, needle: char) -> Option<usize> {
    (from..src.len()).find(|&i| src[i] == needle)
}

fn find_seq(src: &[char], from: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || src.len() < needle.len() {
        return None;
    }
    (from..=src.len() - needle.len()).find(|&i| &src[i..i + needle.len()] == needle)
}

/// The sticky `export const meta = {` matcher, tested only at an offset the
/// scanner already knows is real code. Returns the offset of the `{`.
fn match_decl(src: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    let word = |i: &mut usize, w: &str| -> bool {
        for expected in w.chars() {
            if src.get(*i) != Some(&expected) {
                return false;
            }
            *i += 1;
        }
        true
    };
    let space = |i: &mut usize, required: bool| -> bool {
        let start = *i;
        while matches!(
            src.get(*i),
            Some(' ') | Some('\t') | Some('\r') | Some('\n')
        ) {
            *i += 1;
        }
        !required || *i > start
    };
    if !word(&mut i, "export") || !space(&mut i, true) {
        return None;
    }
    if !word(&mut i, "const") || !space(&mut i, true) {
        return None;
    }
    if !word(&mut i, "meta") {
        return None;
    }
    space(&mut i, false);
    if src.get(i) != Some(&'=') {
        return None;
    }
    i += 1;
    space(&mut i, false);
    if src.get(i) != Some(&'{') {
        return None;
    }
    Some(i)
}

/// Locate the `export const meta = {…}` statement, skipping string bodies,
/// template bodies and comments so a commented-out or quoted declaration cannot
/// be mistaken for the real one. `None` when the script declares none.
///
/// An unterminated string or comment OUTSIDE `meta` just ends the search (the
/// engine's syntax check names it better); one INSIDE the literal is an error,
/// from [`scan_balanced`].
pub fn meta_span(script: &str) -> Result<Option<MetaSpan>, BoughError> {
    let src: Vec<char> = script.chars().collect();
    let mut stack: Vec<Frame> = vec![Frame::Code { depth: 0 }];
    let mut i = 0usize;
    while i < src.len() {
        let c = src[i];
        let top = stack.last_mut().expect("stack is never empty");

        if let Frame::Template = top {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '`' {
                stack.pop();
                i += 1;
                continue;
            }
            if c == '$' && src.get(i + 1) == Some(&'{') {
                stack.push(Frame::Code { depth: 0 });
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if c == '"' || c == '\'' {
            match skip_quoted(&src, i) {
                None => return Ok(None),
                Some(next) => {
                    i = next;
                    continue;
                }
            }
        }
        if c == '`' {
            stack.push(Frame::Template);
            i += 1;
            continue;
        }
        if c == '/' && src.get(i + 1) == Some(&'/') {
            match find_char(&src, i, '\n') {
                None => return Ok(None),
                Some(nl) => {
                    i = nl + 1;
                    continue;
                }
            }
        }
        if c == '/' && src.get(i + 1) == Some(&'*') {
            match find_seq(&src, i + 2, &['*', '/']) {
                None => return Ok(None),
                Some(end) => {
                    i = end + 2;
                    continue;
                }
            }
        }
        if c == '}' && matches!(top, Frame::Code { depth } if *depth == 0) && stack.len() > 1 {
            stack.pop();
            i += 1;
            continue;
        }
        if c == 'e' && !(i > 0 && is_ident(src[i - 1])) {
            if let Some(literal_start) = match_decl(&src, i) {
                let end = scan_balanced_chars(&src, literal_start)?;
                return Ok(Some(MetaSpan {
                    start: i,
                    literal_start,
                    end,
                    literal: src[literal_start..end].iter().collect(),
                }));
            }
        }
        if let Frame::Code { depth } = stack.last_mut().expect("stack is never empty") {
            if c == '{' {
                *depth += 1;
            } else if c == '}' {
                *depth -= 1;
            }
        }
        i += 1;
    }
    Ok(None)
}

/// The literal text of `export const meta = {…}`, `{` through `}`, or `None`.
pub fn meta_literal(script: &str) -> Result<Option<String>, BoughError> {
    Ok(meta_span(script)?.map(|s| s.literal))
}

// ---------------------------------------------------------------------------
// The parser (half 2: read it without executing anything)
// ---------------------------------------------------------------------------

/// Literals nest shallowly; anything deeper is a script, not a literal.
const MAX_DEPTH: usize = 16;

struct Cursor<'a> {
    src: &'a [char],
    i: usize,
    end: usize,
}

/// Whitespace and comments between tokens — the only things allowed to be
/// nothing.
fn trivia(cur: &mut Cursor) -> Result<(), BoughError> {
    while cur.i < cur.end {
        let c = cur.src[cur.i];
        if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
            cur.i += 1;
            continue;
        }
        if c == '/' && cur.src.get(cur.i + 1) == Some(&'/') {
            cur.i = match find_char(cur.src, cur.i, '\n') {
                Some(nl) if nl <= cur.end => nl + 1,
                _ => cur.end,
            };
            continue;
        }
        if c == '/' && cur.src.get(cur.i + 1) == Some(&'*') {
            match find_seq(cur.src, cur.i + 2, &['*', '/']) {
                None => return Err(malformed("a block comment is never closed", cur.src, cur.i)),
                Some(close) => {
                    cur.i = close + 2;
                    continue;
                }
            }
        }
        return Ok(());
    }
    Ok(())
}

/// JS string escapes, decoded. An unknown escape is its own character, as in JS.
fn unescape(src: &[char], from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut i = from;
    while i < to {
        let c = src[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = src.get(i) else { break };
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            '\n' => {} // line continuation
            'x' => {
                let hex: String = src[(i + 1).min(src.len())..(i + 3).min(src.len())]
                    .iter()
                    .collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
                i += 2;
            }
            'u' => {
                if src.get(i + 1) == Some(&'{') {
                    let close = find_char(src, i, '}').unwrap_or(to);
                    let hex: String = src[(i + 2).min(src.len())..close.min(src.len())]
                        .iter()
                        .collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                    i = close;
                } else {
                    let hex: String = src[(i + 1).min(src.len())..(i + 5).min(src.len())]
                        .iter()
                        .collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                    i += 4;
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    out
}

/// A quoted or backtick string. A template carrying `${…}` is computed, not a
/// value.
fn parse_string(cur: &mut Cursor) -> Result<String, BoughError> {
    let at = cur.i;
    let quote = cur.src[at];
    if quote == '`' {
        let mut i = at + 1;
        while i < cur.end {
            let c = cur.src[i];
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '$' && cur.src.get(i + 1) == Some(&'{') {
                return Err(computed(
                    "a template literal with `${…}` interpolation",
                    cur.src,
                    i,
                ));
            }
            if c == '`' {
                let text = unescape(cur.src, at + 1, i);
                cur.i = i + 1;
                return Ok(text);
            }
            i += 1;
        }
        return Err(malformed("a template literal is never closed", cur.src, at));
    }
    match skip_quoted(cur.src, at) {
        Some(next) if next <= cur.end => {
            cur.i = next;
            Ok(unescape(cur.src, at + 1, next - 1))
        }
        _ => Err(malformed("a string is never closed", cur.src, at)),
    }
}

/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`, sticky at `at`.
fn match_number(src: &[char], at: usize) -> Option<String> {
    let mut i = at;
    let digit = |i: usize| src.get(i).is_some_and(|c| c.is_ascii_digit());
    if src.get(i) == Some(&'-') {
        i += 1;
    }
    if src.get(i) == Some(&'0') {
        i += 1;
    } else if src.get(i).is_some_and(|c| ('1'..='9').contains(c)) {
        i += 1;
        while digit(i) {
            i += 1;
        }
    } else {
        return None;
    }
    if src.get(i) == Some(&'.') && digit(i + 1) {
        i += 1;
        while digit(i) {
            i += 1;
        }
    }
    if matches!(src.get(i), Some('e') | Some('E')) {
        let mut j = i + 1;
        if matches!(src.get(j), Some('+') | Some('-')) {
            j += 1;
        }
        if digit(j) {
            i = j;
            while digit(i) {
                i += 1;
            }
        }
    }
    Some(src[at..i].iter().collect())
}

/// `[A-Za-z_$][A-Za-z0-9_$]*`, sticky at `at`.
fn match_key(src: &[char], at: usize) -> Option<String> {
    let first = *src.get(at)?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    let mut i = at + 1;
    while src.get(i).copied().is_some_and(is_ident) {
        i += 1;
    }
    Some(src[at..i].iter().collect())
}

fn parse_value(cur: &mut Cursor, depth: usize) -> Result<Value, BoughError> {
    if depth > MAX_DEPTH {
        return Err(malformed("the literal nests too deeply", cur.src, cur.i));
    }
    trivia(cur)?;
    if cur.i >= cur.end {
        return Err(malformed("the literal ends early", cur.src, cur.i));
    }
    let at = cur.i;
    let c = cur.src[at];

    if c == '{' {
        return parse_object(cur, depth);
    }
    if c == '[' {
        return parse_array(cur, depth);
    }
    if c == '"' || c == '\'' || c == '`' {
        return Ok(Value::String(parse_string(cur)?));
    }

    if c == '-' || c.is_ascii_digit() {
        let Some(text) = match_number(cur.src, at) else {
            return Err(computed("a numeric expression", cur.src, at));
        };
        cur.i = at + text.chars().count();
        // Integral literals stay integers, exactly as `JSON.parse` gives them.
        return match text.parse::<i64>() {
            Ok(i) => Ok(Value::Number(i.into())),
            Err(_) => text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .ok_or_else(|| computed("a numeric expression", cur.src, at)),
        };
    }

    if let Some(word) = match_key(cur.src, at) {
        cur.i = at + word.chars().count();
        return match word.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "null" => Ok(Value::Null),
            "undefined" => Err(computed("`undefined`", cur.src, at)),
            // A bare name: a variable read, or the callee of a call. Both are
            // the same mistake and the same fix.
            other => Err(computed(&format!("`{other}`"), cur.src, at)),
        };
    }
    if c == '.' {
        return Err(computed("a `...` spread", cur.src, at));
    }
    let snippet: String = cur.src[at..(at + 16).min(cur.end)]
        .iter()
        .collect::<String>()
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string();
    Err(computed(&format!("`{snippet}`"), cur.src, at))
}

fn parse_array(cur: &mut Cursor, depth: usize) -> Result<Value, BoughError> {
    let mut out: Vec<Value> = Vec::new();
    cur.i += 1; // [
    loop {
        trivia(cur)?;
        if cur.i >= cur.end {
            return Err(malformed("an array is never closed", cur.src, cur.i));
        }
        if cur.src[cur.i] == ']' {
            cur.i += 1;
            return Ok(Value::Array(out));
        }
        if cur.src[cur.i] == ',' {
            // `[a, , b]` is a hole — legal JS, not expressible in the meta we accept.
            return Err(malformed("an array hole (`,,`)", cur.src, cur.i));
        }
        out.push(parse_value(cur, depth + 1)?);
        trivia(cur)?;
        match cur.src.get(cur.i) {
            Some(',') => {
                cur.i += 1;
                continue;
            }
            Some(']') => {
                cur.i += 1;
                return Ok(Value::Array(out));
            }
            // Anything else is an operator joining two values: `a + b`,
            // `xs.concat(y)`.
            _ => return Err(computed("an expression", cur.src, cur.i)),
        }
    }
}

fn parse_object(cur: &mut Cursor, depth: usize) -> Result<Value, BoughError> {
    let mut out = Map::new();
    cur.i += 1; // {
    loop {
        trivia(cur)?;
        if cur.i >= cur.end {
            return Err(malformed("an object is never closed", cur.src, cur.i));
        }
        if cur.src[cur.i] == '}' {
            cur.i += 1;
            return Ok(Value::Object(out));
        }

        let key_at = cur.i;
        let c = cur.src[key_at];
        if c == '.' {
            return Err(computed("a `...` spread", cur.src, key_at));
        }
        if c == '[' {
            return Err(computed("a computed key `[…]`", cur.src, key_at));
        }

        let key = if c == '"' || c == '\'' || c == '`' {
            parse_string(cur)?
        } else {
            match match_key(cur.src, key_at) {
                None => return Err(malformed("a property name was expected", cur.src, key_at)),
                Some(k) => {
                    cur.i = key_at + k.chars().count();
                    k
                }
            }
        };

        trivia(cur)?;
        match cur.src.get(cur.i) {
            Some('(') => return Err(computed(&format!("the method `{key}()`"), cur.src, cur.i)),
            Some(',') | Some('}') => {
                return Err(computed(
                    &format!("the shorthand property `{key}`"),
                    cur.src,
                    key_at,
                ))
            }
            Some(':') => cur.i += 1,
            _ => {
                return Err(malformed(
                    "a `:` was expected after a property name",
                    cur.src,
                    cur.i,
                ))
            }
        }

        // A literal `__proto__` key is DATA here — `serde_json::Map` has no
        // prototype to swap, which is what the TS `defineProperty` bought.
        let value = parse_value(cur, depth + 1)?;
        out.insert(key, value);

        trivia(cur)?;
        match cur.src.get(cur.i) {
            Some(',') => {
                cur.i += 1;
                continue;
            }
            Some('}') => {
                cur.i += 1;
                return Ok(Value::Object(out));
            }
            _ => return Err(computed("an expression", cur.src, cur.i)),
        }
    }
}

/// Parse one pure JS literal out of `src[start..end)` (char offsets). Exported
/// for the tests and for anything else that needs "read this literal, run
/// nothing".
pub fn parse_literal(src: &str, start: usize, end: usize) -> Result<Value, BoughError> {
    let chars: Vec<char> = src.chars().collect();
    let end = end.min(chars.len());
    let mut cur = Cursor {
        src: &chars,
        i: start,
        end,
    };
    let value = parse_value(&mut cur, 0)?;
    trivia(&mut cur)?;
    if cur.i < end {
        return Err(malformed("trailing text after the literal", &chars, cur.i));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------------

/// Validate the parsed literal against the meta shape. The issues are
/// per-field, `path: message`, joined by `; ` — the TS zod contract, which the
/// tests grep by field path (`name:`, `phases.0.title`, `phasez`).
fn validate(raw: &Value) -> Result<WorkflowMeta, String> {
    let Value::Object(map) = raw else {
        return Err("meta: expected an object".to_string());
    };
    let mut issues: Vec<String> = Vec::new();

    for key in map.keys() {
        if key != "name" && key != "description" && key != "phases" {
            issues.push(format!("meta: unrecognized key `{key}`"));
        }
    }
    let text = |field: &str, max: usize, issues: &mut Vec<String>| -> Option<String> {
        match map.get(field) {
            None => {
                issues.push(format!("{field}: required"));
                None
            }
            Some(Value::String(s)) if s.is_empty() => {
                issues.push(format!("{field}: must not be empty"));
                None
            }
            Some(Value::String(s)) if s.chars().count() > max => {
                issues.push(format!("{field}: must be at most {max} characters"));
                None
            }
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                issues.push(format!("{field}: expected a string"));
                None
            }
        }
    };
    let name = text("name", 80, &mut issues);
    let description = text("description", 500, &mut issues);

    let mut phases: Option<Vec<WorkflowPhase>> = None;
    match map.get("phases") {
        None => {}
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let Value::Object(p) = item else {
                    issues.push(format!("phases.{i}: expected an object"));
                    continue;
                };
                for key in p.keys() {
                    if key != "title" && key != "detail" {
                        issues.push(format!("phases.{i}: unrecognized key `{key}`"));
                    }
                }
                let title = match p.get("title") {
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => {
                        issues.push(format!("phases.{i}.title: expected a string"));
                        None
                    }
                    None => {
                        issues.push(format!("phases.{i}.title: required"));
                        None
                    }
                };
                let detail = match p.get("detail") {
                    Some(Value::String(s)) => Some(s.clone()),
                    None | Some(Value::Null) => None,
                    Some(_) => {
                        issues.push(format!("phases.{i}.detail: expected a string"));
                        None
                    }
                };
                if let Some(title) = title {
                    out.push(WorkflowPhase { title, detail });
                }
            }
            phases = Some(out);
        }
        Some(_) => issues.push("phases: expected an array".to_string()),
    }

    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    Ok(WorkflowMeta {
        name: name.expect("no issues means name parsed"),
        description: description.expect("no issues means description parsed"),
        phases,
    })
}

/// Extract and validate a script's `meta`. Fails with `WorkflowScriptError`
/// (400) and a message the author can act on: missing, computed, unparseable,
/// or shaped wrong.
pub fn extract_meta(script: &str) -> Result<WorkflowMeta, BoughError> {
    let Some(span) = meta_span(script)? else {
        return Err(BoughError::workflow_script(
            "workflow script must declare `export const meta = {name, description, phases?}` \
             as a pure literal. The host reads it without running the script — it names the \
             run and labels its phases before the first agent starts.",
        ));
    };
    let raw = parse_literal(script, span.literal_start, span.end)?;
    validate(&raw).map_err(|issues| {
        let chars: Vec<char> = script.chars().collect();
        BoughError::workflow_script(format!(
            "invalid workflow meta (line {}): {issues} — meta is \
             {{name, description, phases?: [{{title, detail?}}]}}.",
            line_of(&chars, span.start)
        ))
    })
}

/// The script with its `meta` statement blanked out — the body the worker runs.
///
/// Blanked, not deleted: every character is replaced by a space and every
/// newline kept, so a syntax error's line and column still match the script the
/// author wrote and the file mirrored to `~/.bough/workflows/<id>.js`. Removing
/// the statement is what makes the body compilable at all: `export` is illegal
/// inside the function body the workflow worker builds.
pub fn strip_meta(script: &str) -> String {
    let Ok(Some(span)) = meta_span(script) else {
        return script.to_string();
    };
    let chars: Vec<char> = script.chars().collect();
    let head: String = chars[..span.start].iter().collect();
    let blanked: String = chars[span.start..span.end]
        .iter()
        .map(|c| if *c == '\n' { '\n' } else { ' ' })
        .collect();
    let tail: String = chars[span.end..].iter().collect();
    format!("{head}{blanked}{tail}")
}

/// Both halves at once: the validated meta and the body to run.
pub fn read_workflow_meta(script: &str) -> Result<(WorkflowMeta, String), BoughError> {
    Ok((extract_meta(script)?, strip_meta(script)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four brace hazards in one script: string, template, line and block
    /// comment (`meta.test.ts`'s HAZARDS, verbatim).
    const HAZARDS: &str = r#"// a stray { in a line comment, plus export const meta = { decoy
/* and a { in a block comment } */
export const meta = {
  name: 'audit-handlers',            // trailing { comment }
  description: "matches {a} and {b}, 'quoted' \"too\"",
  phases: [
    { title: `Review \${'x'} {y}` },  /* { block } */
    { title: 'Verify', detail: 'second pass' },
  ],
}
const rest = { not: "meta" }
return rest
"#;

    fn literal(script: &str) -> Option<String> {
        meta_literal(script).expect("no scan error")
    }

    // ---- the scan ---------------------------------------------------------

    #[test]
    fn braces_in_a_string_do_not_end_the_literal() {
        let lit = literal(
            "export const meta = { name: 'x', description: \"has {braces} and } more\" }\n\
             const rest = 1",
        )
        .expect("a literal");
        assert!(lit.starts_with('{') && lit.ends_with('}'), "{lit}");
        assert!(lit.contains("has {braces} and } more"), "{lit}");
        assert!(!lit.contains("rest"), "{lit}");
    }

    #[test]
    fn a_template_literal_with_an_interpolation_does_not_end_it() {
        // The interpolation contributes a `{` and a `}` of its own, and nests a
        // string and a template that each carry more braces. A scan that treats
        // a backtick as an ordinary quote closes at the inner backtick.
        let script = "export const meta = {\n  name: `run ${ { a: `${'}'}` }.a } {x}`,\n  \
                      description: 'after',\n}\nconst rest = 1\n";
        let lit = literal(script).expect("a literal");
        assert!(lit.contains("description: 'after'"), "{lit}");
        assert!(!lit.contains("rest"), "{lit}");
        assert!(lit.ends_with('}'), "{lit}");
    }

    #[test]
    fn braces_inside_line_and_block_comments_are_skipped() {
        let lit = literal(HAZARDS).expect("a literal");
        assert!(lit.contains("trailing { comment }"), "{lit}");
        assert!(lit.contains("/* { block } */"), "{lit}");
        assert!(lit.contains("'second pass'"), "{lit}");
        assert!(!lit.contains("not:"), "{lit}");
    }

    #[test]
    fn a_commented_out_or_quoted_declaration_is_not_mistaken_for_it() {
        let span = meta_span(HAZARDS).unwrap().expect("a span");
        // Line 3, not the decoy on line 1.
        let before: String = HAZARDS.chars().take(span.start).collect();
        assert_eq!(before.split('\n').count(), 3);

        let quoted = "const doc = \"export const meta = { name: 'fake' }\"\n\
                      export const meta = { name: 'real', description: 'd' }\n";
        assert_eq!(extract_meta(quoted).unwrap().name, "real");

        assert_eq!(literal("const meta = {}"), None, "must be exported");
        assert_eq!(
            literal("// export const meta = { name: 'x' }\nreturn 1"),
            None
        );
        assert_eq!(literal("return 1"), None);
    }

    #[test]
    fn an_unterminated_literal_is_an_error_not_a_short_literal() {
        let err = meta_literal("export const meta = { name: 'x',\n  description: 'y'\n")
            .expect_err("never closed");
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "WorkflowScriptError");
        assert!(err.to_string().contains("never closed"), "{err}");

        let err = meta_literal("export const meta = { name: 'x\n }").expect_err("unterminated");
        assert!(err.to_string().contains("single-quoted string"), "{err}");
    }

    // ---- the parse --------------------------------------------------------

    #[test]
    fn extract_meta_reads_the_whole_literal_comments_and_escapes_and_all() {
        let meta = extract_meta(HAZARDS).expect("valid meta");
        assert_eq!(meta.name, "audit-handlers");
        assert_eq!(meta.description, "matches {a} and {b}, 'quoted' \"too\"");
        assert_eq!(
            meta.phases,
            Some(vec![
                WorkflowPhase {
                    title: "Review ${'x'} {y}".into(),
                    detail: None
                },
                WorkflowPhase {
                    title: "Verify".into(),
                    detail: Some("second pass".into())
                },
            ])
        );
    }

    #[test]
    fn an_interpolation_free_template_is_accepted_and_escapes_decode() {
        let meta = extract_meta(
            "export const meta = {\n  name: `audit`,\n  \
             description: \"line\\none\\ttab \\u2713 \\x41\",\n}\n",
        )
        .expect("valid meta");
        assert_eq!(meta.name, "audit");
        assert_eq!(meta.description, "line\none\ttab \u{2713} A");
    }

    #[test]
    fn trailing_commas_and_no_phases_are_fine() {
        let meta = extract_meta("export const meta = { name: 'n', description: 'd', }\n").unwrap();
        assert_eq!(meta.phases, None);
    }

    /// The row-3.7 gate: the scanner CANNOT execute code. Every computed shape
    /// is refused, by name, with the reason and the fix.
    #[test]
    fn every_computed_value_is_rejected_saying_why() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "a variable",
                "export const meta = { name: NAME, description: 'd' }",
                "`NAME`",
            ),
            (
                "a call",
                "export const meta = { name: nameFor('x'), description: 'd' }",
                "`nameFor`",
            ),
            (
                "concatenation",
                "export const meta = { name: 'a' + 'b', description: 'd' }",
                "computed",
            ),
            (
                "interpolation",
                "export const meta = { name: `audit ${target}`, description: 'd' }",
                "interpolation",
            ),
            (
                "a spread",
                "export const meta = { ...defaults, description: 'd' }",
                "spread",
            ),
            (
                "a shorthand property",
                "export const meta = { name, description: 'd' }",
                "shorthand",
            ),
            (
                "a computed key",
                "export const meta = { [key]: 'x', description: 'd' }",
                "computed key",
            ),
            (
                "a method",
                "export const meta = { name() { return 'x' }, description: 'd' }",
                "method",
            ),
            (
                "a call inside phases",
                "export const meta = { name: 'n', description: 'd', phases: phasesFor(1) }",
                "`phasesFor`",
            ),
            (
                "an expression inside an array",
                "export const meta = { name: 'n', description: 'd', \
                 phases: [{ title: 'a' }].concat(b) }",
                "computed",
            ),
        ];
        for (what, script, expected) in cases {
            let err = extract_meta(script).expect_err(what);
            assert_eq!(err.status(), 400, "{what}");
            assert_eq!(err.name(), "WorkflowScriptError", "{what}");
            let msg = err.to_string();
            assert!(msg.contains(expected), "{what}: {msg}");
            // Every computed rejection carries the reason and the fix, not just
            // "invalid" — this message is what the author acts on.
            assert!(msg.contains("pure literal"), "{what}: {msg}");
            assert!(msg.contains("never runs the script"), "{what}: {msg}");
            assert!(msg.contains("line "), "{what}: {msg}");
        }
    }

    /// Nothing in the literal can reach a prototype: the value is data.
    #[test]
    fn a_proto_key_is_data() {
        let src = r#"{ "__proto__": { "polluted": true } }"#;
        let value = parse_literal(src, 0, src.chars().count()).unwrap();
        assert_eq!(value["__proto__"], serde_json::json!({"polluted": true}));
    }

    // ---- shape validation -------------------------------------------------

    #[test]
    fn a_missing_meta_names_the_declaration_the_author_must_write() {
        let err = extract_meta("phase('Review')\nreturn 1\n").expect_err("no meta");
        assert!(
            err.to_string()
                .contains("export const meta = {name, description, phases?}"),
            "{err}"
        );
    }

    #[test]
    fn a_wrong_shape_is_reported_per_field() {
        let err = extract_meta("export const meta = { name: 'x' }\nreturn 1").expect_err("shape");
        assert!(err.to_string().contains("invalid workflow meta"), "{err}");

        for (script, needle) in [
            (
                "export const meta = { name: '', description: 'd' }",
                "name:",
            ),
            (
                "export const meta = { name: 'n', description: 'd', phases: [{}] }",
                "phases.0.title",
            ),
            // A dropped unknown key would produce a run with no phases and no
            // complaint.
            (
                "export const meta = { name: 'n', description: 'd', phasez: [] }",
                "phasez",
            ),
            (
                "export const meta = { name: 42, description: 'd' }",
                "name:",
            ),
        ] {
            let err = extract_meta(script).expect_err(script);
            assert!(err.to_string().contains(needle), "{script}: {err}");
        }
    }

    // ---- stripping --------------------------------------------------------

    #[test]
    fn strip_meta_removes_the_statement_and_keeps_every_line_number() {
        let body = strip_meta(HAZARDS);
        assert_eq!(body.split('\n').count(), HAZARDS.split('\n').count());
        assert!(!body.contains("audit-handlers"));
        let line = |s: &str, n: usize| s.split('\n').nth(n - 1).unwrap_or_default().to_string();
        // The statement's lines (3–10) are blanked, not deleted...
        for n in 3..=10 {
            assert_eq!(line(&body, n).trim(), "", "line {n}");
        }
        // ...and everything outside it survives verbatim, on its original line.
        for n in [1, 2, 11, 12] {
            assert_eq!(line(&body, n), line(HAZARDS, n), "line {n}");
        }
        assert!(body.contains(r#"const rest = { not: "meta" }"#));
    }

    #[test]
    fn a_script_with_no_meta_is_returned_untouched() {
        let script = "phase('Review')\nreturn 1\n";
        assert_eq!(strip_meta(script), script);
    }

    #[test]
    fn read_workflow_meta_gives_the_validated_meta_and_the_runnable_body() {
        let (meta, body) = read_workflow_meta(HAZARDS).expect("both halves");
        assert_eq!(meta.name, "audit-handlers");
        assert_eq!(meta.phases.as_ref().map(Vec::len), Some(2));
        // The statement is gone from the body — the decoy in line 1's comment
        // is the body's own text and stays, which is why this asserts on the
        // literal's content and not on the word `export`.
        assert!(!body.contains("audit-handlers"), "{body}");
        assert!(body.contains("// a stray { in a line comment"), "{body}");
    }
}
