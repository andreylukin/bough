//! Invariant: `$BOUGH_HOME/env` is loaded ONCE, at launch, before anything composes — so a key
//! written there reaches both readers (`!!expr env(...)` snapshots the environment at COMPOSE
//! time; `llm-anthropic`/`llm-openai` read theirs at CALL time) — and the PROCESS ENVIRONMENT
//! WINS: a variable already set when bough started is never overwritten by the file, so a shell
//! export or a test's isolation stays authoritative. This is what makes the routing error's own
//! advice ("put it in ~/.bough/env") true.
//!
//! The file is plain `KEY=VALUE` lines (an optional `export ` prefix and surrounding quotes are
//! tolerated, because the same file is `source`d by the Makefile's `set -a; . file`); `#`
//! comments and blank lines are skipped; anything else is reported and skipped, never a boot
//! failure — a missing or odd env file must not keep an offline machine from booting.

use std::path::Path;

/// PURE: the assignments a file's text carries, in order.
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            tracing::warn!(target: "bough", line, "$BOUGH_HOME/env: not KEY=VALUE; skipped");
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            tracing::warn!(target: "bough", line, "$BOUGH_HOME/env: not a variable name; skipped");
            continue;
        }
        // The same value the shell's `set -a; . file` produces: a QUOTED value keeps everything
        // inside the quotes; an UNQUOTED one ends at the first whitespace, which is also what
        // makes a trailing `  # comment` a comment and not part of a key.
        let value = value.trim();
        let value = match unquote(value) {
            unquoted if unquoted.len() != value.len() => unquoted,
            _ => value.split_whitespace().next().unwrap_or(""),
        };
        out.push((key.to_string(), value.to_string()));
    }
    out
}

/// One matching pair of surrounding quotes, at most.
fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// PURE: which of `pairs` should be set, given what is already in the environment. FIRST spelling
/// of a key in the file wins among duplicates; anything already set wins over the file.
pub fn to_set(
    pairs: Vec<(String, String)>,
    already_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let mut seen: Vec<String> = Vec::new();
    pairs
        .into_iter()
        .filter(|(k, _)| {
            if already_set(k) || seen.contains(k) {
                return false;
            }
            seen.push(k.clone());
            true
        })
        .collect()
}

/// Load `path` into the process environment. Returns the NAMES set (never the values: this line
/// is logged). A missing file is the ordinary state and returns empty.
pub fn load(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let pairs = to_set(parse(&text), |k| std::env::var_os(k).is_some());
    let names: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
    for (k, v) in pairs {
        std::env::set_var(k, v);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_shapes_the_makefile_sources_all_parse() {
        let text = "# keys\nANTHROPIC_API_KEY=sk-ant-x\nexport OPENAI_API_KEY=\"sk-proj-y\"\n\nODD LINE\nLINEAR_API_KEY='lin_z'\n";
        assert_eq!(
            parse(text),
            vec![
                ("ANTHROPIC_API_KEY".into(), "sk-ant-x".into()),
                ("OPENAI_API_KEY".into(), "sk-proj-y".into()),
                ("LINEAR_API_KEY".into(), "lin_z".into()),
            ]
        );
    }

    #[test]
    fn the_process_environment_wins_and_the_first_file_spelling_wins() {
        let pairs = vec![
            ("A".to_string(), "file-a".to_string()),
            ("B".to_string(), "file-b1".to_string()),
            ("B".to_string(), "file-b2".to_string()),
        ];
        let set = to_set(pairs, |k| k == "A");
        assert_eq!(set, vec![("B".to_string(), "file-b1".to_string())]);
    }

    #[test]
    fn a_value_reads_the_way_the_shell_would_read_it() {
        assert_eq!(
            parse("URL=https://x/y?a=1&b=2"),
            vec![("URL".into(), "https://x/y?a=1&b=2".into())]
        );
        // An unquoted value ends at the first whitespace — which is exactly how a trailing
        // inline comment behaves under `set -a; . file` (found live: a commented key line
        // parsed as key+comment and answered 401).
        assert_eq!(
            parse("OPENAI_API_KEY=sk-proj-abc          # used for bough"),
            vec![("OPENAI_API_KEY".into(), "sk-proj-abc".into())]
        );
        // A QUOTED value keeps its spaces and its `#`.
        assert_eq!(
            parse(r##"MSG="hello # world""##),
            vec![("MSG".into(), "hello # world".into())]
        );
    }
}
