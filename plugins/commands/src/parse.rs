//! Invariant: parsing is PURE and total. It never resolves, never dispatches and never reads a
//! registry; a line either is a command line or is text, and that verdict depends on the line and
//! the prefix alone.

use crate::{CommandName, Invocation};

/// PURE. `None` when the line does not start with the prefix; a doubled prefix (`//x`) is
/// literal text and yields `None`, so a message can begin with a slash.
pub fn parse(line: &str, prefix: char) -> Option<Invocation> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(prefix)?;
    // A doubled prefix is the ESCAPE, not a command: `//deploy` is a message that starts with a
    // slash. Checked before the name, so `//` never becomes a command named `/`.
    if rest.starts_with(prefix) {
        return None;
    }
    let rest = rest.trim_end();
    let (name, tail) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if name.is_empty() {
        // A bare prefix is text: there is no command to resolve.
        return None;
    }
    Some(Invocation {
        name: CommandName::new(name),
        raw: line.to_string(),
        args: split_args(tail),
    })
}

/// Shell-style split of a command's argument tail: quoted runs stay whole.
///
/// Total: an unterminated quote closes at end of line rather than failing, because a half-typed
/// line is a normal thing to see and a parse error would be a worse answer than the obvious one.
pub fn split_args(tail: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut open: Option<char> = None;
    let mut started = false;
    for c in tail.chars() {
        match open {
            Some(q) if c == q => {
                open = None;
            }
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                open = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                started = true;
                cur.push(c);
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// The nearest registered name to an unknown one, by edit distance, or `None` when nothing is
/// close enough to be a suggestion rather than a guess.
pub fn did_you_mean(name: &str, known: &[CommandName]) -> Option<String> {
    // A third of the typed name, at least one edit: "fcus" suggests "focus", "xyzzy" suggests
    // nothing. A guess dressed as a suggestion is worse than no suggestion.
    let budget = (name.chars().count() / 3).max(1);
    known
        .iter()
        .map(|k| (distance(name, k.as_str()), k))
        .filter(|(d, _)| *d <= budget)
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())))
        .map(|(_, k)| k.to_string())
}

/// Levenshtein distance, two rows.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_run_stays_whole() {
        assert_eq!(
            split_args("  sol \"two words\" 'and more' tail"),
            vec!["sol", "two words", "and more", "tail"]
        );
        // An empty quoted argument is an argument.
        assert_eq!(split_args(r#" "" x"#), vec!["", "x"]);
    }

    #[test]
    fn a_bare_prefix_is_not_a_command() {
        assert_eq!(parse("/", '/'), None);
        assert_eq!(parse("   ", '/'), None);
        assert_eq!(parse("hello", '/'), None);
    }

    #[test]
    fn a_suggestion_is_never_a_guess() {
        let known = [CommandName::new("focus"), CommandName::new("quit")];
        assert_eq!(did_you_mean("fcus", &known), Some("focus".into()));
        assert_eq!(did_you_mean("zzzzzzzz", &known), None);
    }
}
