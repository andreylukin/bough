//! The TUI's command line (port of `src/tui/args.ts`).
//!
//! The rule is `cli/exec`'s rule, for the same reason: **an unknown flag is an
//! error.** A typo that silently starts anyway is worse than one that stops, and
//! a flag this app does not implement is indistinguishable from a typo. bough
//! edits the real checkout with the user's authority and no sandbox, so a
//! silently-ignored `-w` means the agent writes to a repository the user did not
//! choose and believes it is not touching.
//!
//! Pure and total, so the whole surface is asserted without a terminal. No clap:
//! the grammar is four tokens and USAGE is product text, ported verbatim.

/// Verbatim from the TS source — product surface, including the posture line.
pub const USAGE: &str = "usage: bough [-w DIR]\n\
\n\
  -w, --workspace DIR   where new conversations start (default: the cwd)\n\
  -h, --help            this message\n\
\n\
  the server port comes from BOUGH_PORT (default 4321). It is an env var and\n\
  not a flag because the API client is bound at import, before a flag could be\n\
  read — a --port that parsed and did nothing would be the bug this file fixes.\n\
\n\
programs run as you, with your authority — there is no sandbox.";

/// Successfully parsed arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TuiArgs {
    /// Where a new conversation starts. `None` = the process cwd.
    pub workspace: Option<String>,
}

/// The three outcomes of parsing. The caller maps `UsageError` to stderr +
/// exit 2 and `Help` to stdout + exit 0, before any terminal is taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiArgsResult {
    Args(TuiArgs),
    UsageError(String),
    Help,
}

impl TuiArgsResult {
    pub fn is_usage_error(&self) -> bool {
        matches!(self, TuiArgsResult::UsageError(_))
    }
    pub fn is_help_request(&self) -> bool {
        matches!(self, TuiArgsResult::Help)
    }
}

/// The one short flag. `-w` → `--workspace`.
fn long_for_short(short: &str) -> Option<&'static str> {
    match short {
        "w" => Some("workspace"),
        _ => None,
    }
}

fn is_value_flag(name: &str) -> bool {
    name == "workspace"
}

pub fn parse_tui_args<S: AsRef<str>>(argv: &[S]) -> TuiArgsResult {
    let mut workspace: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        let token = argv[i].as_ref();
        let name: String;
        let inline: Option<String>;

        if token == "--help" || token == "-h" {
            return TuiArgsResult::Help;
        }

        if let Some(rest) = token.strip_prefix("--") {
            match rest.find('=') {
                None => {
                    name = rest.to_string();
                    inline = None;
                }
                Some(eq) => {
                    name = rest[..eq].to_string();
                    inline = Some(rest[eq + 1..].to_string());
                }
            }
        } else if token.starts_with('-') && token.len() > 1 {
            let rest = &token[1..];
            let (short, after) = match rest.find('=') {
                None => (rest, None),
                Some(eq) => (&rest[..eq], Some(rest[eq + 1..].to_string())),
            };
            match long_for_short(short) {
                Some(long) => {
                    name = long.to_string();
                    inline = after;
                }
                None => {
                    return TuiArgsResult::UsageError(format!("unknown flag -{short}\n{USAGE}"));
                }
            }
        } else {
            // The TUI takes no positional argument — it is not `bough exec`, and a
            // stray prompt here would otherwise vanish into a screen that ignores it.
            return TuiArgsResult::UsageError(format!(
                "bough takes no positional argument (got \"{token}\").\n\
                 Did you mean: bough exec \"{token}\"?\n{USAGE}"
            ));
        }

        if !is_value_flag(&name) {
            return TuiArgsResult::UsageError(format!("unknown flag --{name}\n{USAGE}"));
        }
        let value = match inline {
            Some(v) => v,
            None => {
                if i + 1 >= argv.len() {
                    return TuiArgsResult::UsageError(format!("--{name} needs a value\n{USAGE}"));
                }
                i += 1;
                argv[i].as_ref().to_string()
            }
        };
        workspace = Some(value);
        i += 1;
    }

    if let Some(ws) = &workspace {
        if ws.trim().is_empty() {
            return TuiArgsResult::UsageError(format!("--workspace needs a path\n{USAGE}"));
        }
    }
    TuiArgsResult::Args(TuiArgs { workspace })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(argv: &[&str]) -> TuiArgs {
        match parse_tui_args(argv) {
            TuiArgsResult::Args(a) => a,
            other => panic!("expected args, got {other:?}"),
        }
    }

    #[test]
    fn w_and_workspace_both_name_where_a_new_conversation_starts() {
        // The bug: `bough -w /other/repo` opened in the cwd and said nothing, which
        // points an unsandboxed agent at a repository the user did not choose.
        assert_eq!(ok(&["-w", "/tmp/x"]).workspace.as_deref(), Some("/tmp/x"));
        assert_eq!(ok(&["--workspace", "/tmp/x"]).workspace.as_deref(), Some("/tmp/x"));
        assert_eq!(ok(&["--workspace=/tmp/x"]).workspace.as_deref(), Some("/tmp/x"));
        assert_eq!(ok(&["-w=/tmp/x"]).workspace.as_deref(), Some("/tmp/x"));
        // No flag at all is still the common case, and still means "the cwd".
        assert_eq!(ok(&[]).workspace, None);
    }

    #[test]
    fn an_unknown_flag_stops_rather_than_starting_anyway() {
        for argv in [&["--wrokspace", "/tmp"][..], &["-q"][..], &["--json"][..]] {
            assert!(
                parse_tui_args(argv).is_usage_error(),
                "{argv:?} should be refused"
            );
        }
        // A flag that needs a value and has none is an error, not an empty string.
        assert!(parse_tui_args(&["-w"]).is_usage_error());
        assert!(parse_tui_args(&["-w", "  "]).is_usage_error());
    }

    #[test]
    fn a_positional_argument_is_refused_and_points_at_bough_exec() {
        // Typing a prompt at the TUI is a real mistake, and silently swallowing it
        // into a screen that ignores it is the unhelpful answer.
        match parse_tui_args(&["fix the tests"]) {
            TuiArgsResult::UsageError(text) => {
                assert!(text.contains("bough exec"), "{text}");
                assert!(text.contains("fix the tests"), "{text}");
            }
            other => panic!("expected usage error, got {other:?}"),
        }
    }

    #[test]
    fn help_is_answered_and_the_usage_states_the_posture() {
        for argv in [&["--help"][..], &["-h"][..], &["-w", "/tmp", "--help"][..]] {
            assert!(parse_tui_args(argv).is_help_request(), "{argv:?}");
        }
        assert!(USAGE.contains("--workspace"));
        // Spec §2 — the same sentence `bough exec --help` prints.
        assert!(USAGE.contains("no sandbox"));
    }

    #[test]
    fn unknown_flag_errors_carry_the_usage_text() {
        match parse_tui_args(&["--json"]) {
            TuiArgsResult::UsageError(text) => {
                assert!(text.starts_with("unknown flag --json\n"), "{text}");
                assert!(text.contains("no sandbox"), "USAGE must ride along: {text}");
            }
            other => panic!("expected usage error, got {other:?}"),
        }
    }
}
