//! (Phase ux1 review: this module lives in `tui-render`, not in `bough-util`. §0.1 enumerates
//! the center exhaustively as "branded ids, home paths, timeouts"; about-line vocabulary is
//! PRESENTATION, and it belongs to the row-less render library both panes already read.)
//! Invariant: an about-line is ONE clean sentence (phase ux1 §2.10, minor 29). It is the most
//! repeated text on the screen, so it carries no markdown markers, no spliced fragments and no
//! dangling emphasis: `read mail \`say hi\`; Hi; ! 👋 ; **` was three fragments and a broken bold.

/// PURE: one clean sentence. Markdown stripped, emoji kept, whitespace collapsed, clipped on a
/// WORD boundary with `…`, never spliced with `;`.
///
/// The rules, in the order they apply:
///
/// 1. **Markers go.** Backticks, `*`, `_`, `#`, `>` and link brackets are removed; the words
///    inside them stay. A dangling `**` therefore cannot survive, because there is no such thing
///    as a marker to leave behind.
/// 2. **One sentence.** Everything from the first `;` onwards is dropped — that separator is what
///    turned `about-line`'s clause fold into a list — and so is everything after the first
///    sentence-ending `.`/`!`/`?` that is followed by whitespace.
/// 3. **Whitespace collapses**, newlines included, so the line is one row by construction.
/// 4. **The clip is on a word boundary**, with `…`, so a cut never lands mid-word.
///
/// An input that reduces to nothing answers the empty string: a caller showing a line at all is
/// the caller's decision, and inventing "nothing to report" here would put words in the agent's
/// mouth.
pub fn one_sentence(raw: &str, max_chars: usize) -> String {
    let stripped = strip_markers(raw);
    let one = first_clause(&stripped);
    let collapsed = collapse(&one);
    clip_on_word(&collapsed, max_chars)
}

/// Markdown markers, removed; the text they wrapped is kept.
fn strip_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '`' | '*' | '_' | '#' | '>' | '[' | ']' => {}
            _ => out.push(c),
        }
    }
    out
}

/// The first clause: up to the first `;`, or the first sentence end followed by space.
fn first_clause(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut end = chars.len();
    for (i, c) in chars.iter().enumerate() {
        if *c == ';' {
            end = i;
            break;
        }
        if matches!(c, '.' | '!' | '?') {
            match chars.get(i + 1) {
                // A terminator at the very end, or one followed by space, ends the sentence. The
                // terminator itself is kept: it is punctuation, not a separator.
                None => {
                    end = i + 1;
                    break;
                }
                Some(n) if n.is_whitespace() => {
                    end = i + 1;
                    break;
                }
                _ => {}
            }
        }
    }
    chars[..end].iter().collect()
}

/// Every run of whitespace becomes one space; the ends are trimmed. Stray separator punctuation
/// left stranded by the marker strip (`read mail  ; ! ;`) is trimmed off the tail too.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            in_ws = true;
            continue;
        }
        if in_ws && !out.is_empty() {
            out.push(' ');
        }
        in_ws = false;
        out.push(c);
    }
    out.trim_matches(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .to_string()
}

/// Clip on a word boundary, marking the cut.
fn clip_on_word(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: Vec<char> = s.chars().take(max_chars.saturating_sub(1)).collect();
    let last_space = head.iter().rposition(|c| *c == ' ');
    let cut: String = match last_space {
        // Only honour the word boundary when it does not throw away most of the line.
        Some(i) if i * 2 > head.len() => head[..i].iter().collect(),
        _ => head.iter().collect(),
    };
    format!("{}…", cut.trim_end())
}
