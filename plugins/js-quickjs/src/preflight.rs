//! Invariant: a syntax error the MODEL can act on beats an engine's bare message. This is main's
//! `preflight.rs` scanner (`git show main:crates/bough-core/src/harness/preflight.rs`) ported
//! verbatim — it finds the unterminated string a bare "unexpected end of input" hides, and it
//! explains a shadowed host binding — with one deviation named in the module: main read its bound
//! names from a global `program_params()`; here they are passed in, because on this branch the
//! injected set is per-agent (the scope snapshot, §0.1) and there is no global list.

use std::sync::OnceLock;

/// A string literal closed by a raw newline. 1-indexed `line`/`col`; `text` is the full source
/// line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnterminatedHit {
    pub line: usize,
    pub col: usize,
    pub text: String,
    pub quote: char,
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Locate a string literal closed by a raw newline.
///
/// This is THE failure mode for model-generated code: the model assembles the program inside a
/// template literal, and every `\n` meant for a string in the GENERATED program is consumed by
/// the outer literal, leaving a real newline inside `"..."`. The engine reports it with no usable
/// position, which tells the author nothing.
///
/// A scanner rather than a regex because it has to skip comments and template literals (with
/// `${}` nesting depth), where a raw newline is perfectly legal.
pub fn unterminated_string(src: &str) -> Option<UnterminatedHit> {
    let s: Vec<char> = src.chars().collect();
    let mut line = 1usize;
    let mut col = 1usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < s.len() {
        let c = s[i];
        let next = s.get(i + 1).copied();
        // Line comment: skip to end of line.
        if c == '/' && next == Some('/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
            line += 1;
            col = 1;
            i += 1;
            continue;
        }
        // Block comment: skip wholesale, counting its newlines.
        if c == '/' && next == Some('*') {
            let end = src_find(&s, i + 2);
            let stop = match end {
                Some(e) => e + 2,
                None => s.len(),
            };
            line += s[i..stop.min(s.len())]
                .iter()
                .filter(|&&ch| ch == '\n')
                .count();
            col = 1;
            i = match end {
                Some(e) => e + 2,
                None => s.len().saturating_add(1),
            };
            continue;
        }
        // Template literals may span lines legally; walk them (with `${}` nesting) so their
        // newlines never look like an unterminated quote.
        if c == '`' {
            i += 1;
            while i < s.len() {
                if s[i] == '\\' {
                    i += 2;
                    continue;
                }
                if s[i] == '\n' {
                    line += 1;
                    col = 1;
                    i += 1;
                    continue;
                }
                if s[i] == '$' && s.get(i + 1) == Some(&'{') {
                    depth += 1;
                } else if s[i] == '}' && depth > 0 {
                    depth -= 1;
                } else if s[i] == '`' && depth == 0 {
                    break;
                }
                i += 1;
            }
            col += 1;
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            let (start_line, start_col) = (line, col);
            i += 1;
            while i < s.len() {
                if s[i] == '\\' {
                    i += 2;
                    continue;
                }
                if s[i] == '\n' || (i == s.len() - 1 && s[i] != c) {
                    let text = src
                        .split('\n')
                        .nth(start_line - 1)
                        .unwrap_or("")
                        .to_string();
                    return Some(UnterminatedHit {
                        line: start_line,
                        col: start_col,
                        text,
                        quote: c,
                    });
                }
                if s[i] == c {
                    break;
                }
                i += 1;
            }
            col += 1;
            i += 1;
            continue;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
        i += 1;
    }
    None
}

/// `indexOf("*/", from)` over the char vector.
fn src_find(s: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < s.len() {
        if s[i] == '*' && s[i + 1] == '/' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Shape the model-facing message for an engine parse failure. `why` is the engine's own
/// SyntaxError message; `code` is the program source; `bound` is the set of names injected into
/// this program's scope. Three shapes, verbatim from main: shadowed bound name, newline-closed
/// string, or the engine's words alone.
pub fn syntax_error_message(why: &str, code: &str, bound: &[String]) -> String {
    // Three phrasings because three engines: JSC (Bun) says "Cannot declare a {let variable,const
    // variable,class} twice: 'x'", V8 says "Identifier 'x' has already been declared", QuickJS
    // says "redeclaration of let 'x'". Matching all three keeps this message — a product surface
    // — from depending on which engine parsed the program.
    static SHADOW: OnceLock<regex::Regex> = OnceLock::new();
    let shadow = SHADOW.get_or_init(|| {
        regex::Regex::new(
            r"Cannot declare an? [a-z ]*twice: '([^']+)'|Identifier '([^']+)' has already been declared|redeclaration of (?:let |const |class |var )?'([^']+)'",
        )
        .expect("static regex")
    });
    if let Some(caps) = shadow.captures(why) {
        let shadowed = caps
            .get(1)
            .or_else(|| caps.get(2))
            .or_else(|| caps.get(3))
            .map(|m| m.as_str());
        // Only a bound name is ours to explain — shadowing anything else is the program's own
        // business (and the engine's own message).
        if let Some(name) = shadowed {
            if bound.iter().any(|b| b == name) {
                let mut chars = name.chars();
                let renamed = match chars.next() {
                    Some(first) => format!("my{}{}", first.to_uppercase(), chars.as_str()),
                    None => "my".to_string(),
                };
                // "host function" would be a lie for `console`, which is bound the same way and
                // is not a bridged call. "already bound" is true of all of them.
                return format!(
                    "program does not parse: {why} — `{name}` is already bound in every \
                     program's scope, so declaring it shadows the binding. Rename your \
                     variable (`{renamed}`) and call `{name}` as it is."
                );
            }
        }
    }
    match unterminated_string(code) {
        None => format!("program does not parse: {why}"),
        Some(hit) => {
            let quote_word = if hit.quote == '"' { "double" } else { "single" };
            format!(
                "program does not parse: {why} — line {line}: a {quote_word}-quoted string is \
                 closed by a real newline.\n  {line} | {snippet}\nIf you built this code inside \
                 a template literal, write \\\\n (escaped) for newlines that belong to the \
                 GENERATED code's strings — a bare \\n is consumed by the outer literal.",
                line = hit.line,
                snippet = clip(hit.text.trim(), 90),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound() -> Vec<String> {
        ["bash", "console", "view"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    // ---- the scanner: newlines that are legal stay legal (main's vm.test.ts) --------------

    #[test]
    fn unterminated_string_finds_the_newline_closed_literal() {
        let hit = unterminated_string("const p = \"one\ntwo\";").expect("a hit");
        assert_eq!(
            hit,
            UnterminatedHit {
                line: 1,
                col: 11,
                text: "const p = \"one".into(),
                quote: '"'
            }
        );
    }

    #[test]
    fn unterminated_string_skips_template_literals_comments_and_fine_strings() {
        assert_eq!(unterminated_string("const t = `a\nb`;\n"), None);
        assert_eq!(unterminated_string("// a 'quote\nconst x = 1;\n"), None);
        assert_eq!(
            unterminated_string("/* a 'quote\nspanning */\nconst x = 1;\n"),
            None
        );
        assert_eq!(unterminated_string("const s = \"fine\";\n"), None);
    }

    #[test]
    fn unterminated_string_walks_template_interpolation_nesting() {
        assert_eq!(unterminated_string("const t = `a${ {b: 1}.b }c\nd`;"), None);
        let hit = unterminated_string("const a = 1;\nconst b = 'x\ny';").expect("a hit");
        assert_eq!(hit.line, 2);
        assert_eq!(hit.quote, '\'');
        assert_eq!(hit.text, "const b = 'x");
    }

    // ---- the three message shapes, pinned against main's real output ---------------------

    #[test]
    fn shadow_message_is_verbatim() {
        let why = "Cannot declare a let variable twice: 'bash'.";
        assert_eq!(
            syntax_error_message(why, "let bash = 1;\nawait bash('x')", &bound()),
            "program does not parse: Cannot declare a let variable twice: 'bash'. — `bash` is \
             already bound in every program's scope, so declaring it shadows the binding. \
             Rename your variable (`myBash`) and call `bash` as it is."
        );
        let v8 = "Identifier 'console' has already been declared";
        let msg = syntax_error_message(v8, "let console = 1;", &bound());
        assert!(msg.contains("`console` is already bound"), "{msg}");
        assert!(msg.contains("myConsole"), "{msg}");
        // QuickJS's own phrasing lands in the same shape — this is the engine that will run it.
        let qjs = "redeclaration of let 'bash'";
        let msg = syntax_error_message(qjs, "let bash = 1;", &bound());
        assert!(msg.contains("`bash` is already bound"), "{msg}");
    }

    #[test]
    fn shadowing_a_non_bound_name_is_not_ours_to_explain() {
        let why = "Cannot declare a let variable twice: 'notAHostFn'.";
        assert_eq!(
            syntax_error_message(why, "let notAHostFn = 1; let notAHostFn = 2;", &bound()),
            "program does not parse: Cannot declare a let variable twice: 'notAHostFn'."
        );
    }

    #[test]
    fn unterminated_message_is_verbatim() {
        assert_eq!(
            syntax_error_message("Unexpected EOF", "const p = \"one\ntwo\";", &bound()),
            "program does not parse: Unexpected EOF — line 1: a double-quoted string is closed \
             by a real newline.\n  1 | const p = \"one\nIf you built this code inside a \
             template literal, write \\\\n (escaped) for newlines that belong to the GENERATED \
             code's strings — a bare \\n is consumed by the outer literal."
        );
    }

    #[test]
    fn plain_parse_failure_carries_the_engine_words() {
        assert_eq!(
            syntax_error_message("Unexpected token '}'", "await bash(", &bound()),
            "program does not parse: Unexpected token '}'"
        );
    }

    #[test]
    fn clip_appends_ellipsis_past_90() {
        let long = "x".repeat(120);
        let clipped = clip(&long, 90);
        assert_eq!(clipped.chars().count(), 91);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("short", 90), "short");
    }
}
