//! The compaction scout (port of `src/history/explore.ts`) — a bash-capable
//! subagent that reads the CURRENT state of the files a span touched, so the
//! summary describes the checkout rather than the conversation's memory of it.
//!
//! WHY THIS EXISTS. `compact.rs` summarizes from the transcript and nothing
//! else, and a transcript is a record of intentions as much as outcomes: a span
//! says "renamed `foo()` to `bar()`", then three turns later a revert put it
//! back, and the summary that replaces the span asserts a rename that is not in
//! the tree. The compacted branch then CONTINUES from that summary — it is the
//! only thing left of those turns — so a wrong fact there is not a cosmetic
//! blemish, it is the next turn's premise.
//!
//! IT IS ENRICHMENT, AND IT NEVER FAILS THE COMPACTION. Every path out of here
//! that is not notes is `None`: no paths found, no key for the scout's model, a
//! provider error, an overrun, a scout that returned nothing. The caller then
//! summarizes exactly as it did before this module existed. That asymmetry is
//! deliberate — compaction is how a user rescues a conversation that has grown
//! too long, and an enrichment step that could take it down would make the
//! rescue less reliable than no rescue at all.
//!
//! SCOPED TO THE DIRECTORIES OF THE FILES THE SPAN TOUCHED, not to the
//! workspace. `touched_paths` mines those paths out of the transcript text —
//! including the `run_steps` program source, which is where a path appears in
//! this harness, since every file verb is a call inside a program rather than a
//! tool call of its own — and keeps only the ones that still exist, because a
//! path that was deleted has nothing to explore and a hallucinated one never
//! had anything.
//!
//! ROUNDS ARE CAPPED AND THE TOOL IS ONE. The scout gets `bash` and nothing
//! else, and `MAX_ROUNDS` rounds to use it in; the round after the cap is asked
//! for its notes with tools forbidden, so an overrun still yields what it
//! learned instead of throwing it away. It runs in the session's own workspace
//! with the user's authority — the same authority every other command in the
//! session already has — but it is briefed to read and not to write.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::hostfn::jobs::JobRegistry;
use crate::hostfn::shell::{create_shell_host_fns, ShellCtx, ShellOptions};
use crate::llm::{client_for, ClientOpts};
use crate::schema::parts::Message;
use crate::types::{
    LlmBlock, LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmRole, LlmToolDef,
};

use super::compact::render_span;

/// The scout's model.
///
/// Pinned rather than inherited, and it is the ONE decision here that is not
/// about safety: the session's own model is whatever the user is paying for
/// their real work, and reading three directories to check whether a rename
/// survived is not that work. Overridable because a user holding no OpenAI key
/// needs a way to name a model they can actually reach — and if they do not,
/// the client fails, this returns `None`, and compaction proceeds unenriched.
pub const DEFAULT_EXPLORE_MODEL: &str = "gpt-5.6-luna";

/// The scout's model from an environment reader, trimmed; empty reads as unset.
pub fn explore_model_from(env: &dyn Fn(&str) -> Option<String>) -> String {
    env("BOUGH_COMPACT_EXPLORE_MODEL")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_EXPLORE_MODEL.to_string())
}

/// The scout's model, read from the process environment per call (never cached
/// — the env is read where it is used, like every other path in this tree).
pub fn explore_model() -> String {
    explore_model_from(&|k| std::env::var(k).ok())
}

/// Rounds the scout may use `bash` in before it is asked to write up.
const MAX_ROUNDS: usize = 6;
/// Wall clock for the whole scouting run. A compaction waits on this.
const TIMEOUT_MS: u64 = 90_000;
/// Directories named in the brief. Beyond a handful the scope stops being a scope.
const MAX_DIRS: usize = 6;
/// One command's output as the scout sees it.
const OUTPUT_CLIP: usize = 4000;
const MAX_TOKENS: i64 = 1024;

const SYSTEM: &str = "You are scouting a codebase for a summarizer. You will be given a span of a \
coding-agent conversation and the directories of the files it touched. Use bash to \
establish what is TRUE OF THE CHECKOUT NOW — whether the changes the span describes \
are present, what shape the code ended up in, what the recent commits say. Read \
only: ls, cat, rg, git log, git diff, sed -n. Never write, never commit, never run \
a build or a test suite. Then answer with terse notes for the summarizer: what is \
actually in the tree, and anything the conversation claims that the tree contradicts. \
Notes only — no preamble, no offer to continue.";

/// A path-shaped token: at least one separator or an extension, and none of the
/// characters that would make it prose. Deliberately loose — the filesystem is
/// the gate.
static PATH_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w./-]*[\w-]+\.[A-Za-z]\w{0,7}\b").expect("static regex"));

/// Paths the span touched that still exist, as workspace-relative strings.
///
/// Mined from the RENDERED transcript rather than from part structure on
/// purpose. A file verb in this harness is `await patch("src/x.ts", …)` inside
/// a `run_steps` program, so the path is a string literal in tool-call input;
/// there is no structured field to read and there never will be while the model
/// acts through programs. A regex over the text plus an existence filter is the
/// honest version of that: the regex over-matches happily (versions, globs,
/// sentences with dots) and the filesystem is what decides.
pub fn touched_paths(span: &[Message], workspace: &str) -> Vec<String> {
    let text = render_span(span);
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for m in PATH_TOKEN.find_iter(&text) {
        let raw = m.as_str();
        if raw.is_empty() || seen.iter().any(|s| s == raw) {
            continue;
        }
        seen.push(raw.to_string());
        let raw_path = Path::new(raw);
        let abs: PathBuf = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            Path::new(workspace).join(raw_path)
        };
        // Inside the workspace only. An absolute path to /etc or to another
        // checkout may be real and is still not this session's subject, and
        // pointing a scout at it would scope the exploration by whatever the
        // transcript happened to mention.
        let Some(rel) = relative_within(workspace, &abs) else {
            continue;
        };
        if rel.is_empty() || !abs.exists() {
            continue;
        }
        out.push(rel);
    }
    out
}

/// `abs` relative to `base`, or `None` when it is not under it. Lexical, like
/// every other confinement decision in this tree.
fn relative_within(base: &str, abs: &Path) -> Option<String> {
    let base = normalize(Path::new(base));
    let abs = normalize(abs);
    let rel = abs.strip_prefix(&base).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// Lexical normalization: resolve `.` and `..` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The directories those files live in — the scout's actual scope.
pub fn touched_dirs(paths: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    for p in paths {
        let d = Path::new(p)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let norm = if d.is_empty() { ".".to_string() } else { d };
        if !dirs.contains(&norm) {
            dirs.push(norm);
        }
        if dirs.len() >= MAX_DIRS {
            break;
        }
    }
    dirs
}

/// What the scout needs. `llm`/`model` are injected in tests; `registry` is the
/// process job registry (absent = a private one, which is right for a scout
/// that never backgrounds anything).
pub struct ExploreCtx {
    pub session_id: String,
    pub workspace: String,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub model: Option<String>,
    pub registry: Option<Arc<JobRegistry>>,
}

fn bash_tool() -> LlmToolDef {
    LlmToolDef {
        name: "bash".to_string(),
        description:
            "Run one read-only shell command in the workspace and get its combined output."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": { "command": { "type": "string", "description": "the command to run" } },
            "required": ["command"],
        }),
    }
}

/// Scout the directories a span touched. Returns notes for the summarizer, or
/// `None` when there is nothing to say or anything at all went wrong (see the
/// header: this step may not fail a compaction).
pub async fn explore_span(ctx: &ExploreCtx, span: &[Message]) -> Option<String> {
    let paths = touched_paths(span, &ctx.workspace);
    if paths.is_empty() {
        return None;
    }
    let dirs = touched_dirs(&paths);

    // The scout's own clock. A compaction is a foreground request the user is
    // waiting on, so a scout that wanders is cut off and the compaction
    // proceeds without it. Deliberately total: every failure here — no key, a
    // 429, a timeout, a shell that could not start — is a compaction that
    // summarizes from the transcript alone, which is what it did before this
    // module existed.
    tokio::time::timeout(
        Duration::from_millis(TIMEOUT_MS),
        scout(ctx, span, &paths, &dirs),
    )
    .await
    .unwrap_or(None)
}

async fn scout(
    ctx: &ExploreCtx,
    span: &[Message],
    paths: &[String],
    dirs: &[String],
) -> Option<String> {
    let model = ctx.model.clone().unwrap_or_else(explore_model);
    let llm = match ctx.llm.clone() {
        Some(llm) => llm,
        None => client_for(&model, ClientOpts::default()),
    };
    let registry = ctx
        .registry
        .clone()
        .unwrap_or_else(|| Arc::new(JobRegistry::new()));
    let shell = create_shell_host_fns(
        ShellCtx {
            session_id: ctx.session_id.clone(),
            workspace: ctx.workspace.clone(),
            ..Default::default()
        },
        ShellOptions::new(registry),
    );
    let cancel = CancellationToken::new();

    let mut messages: Vec<LlmMessage> = vec![LlmMessage {
        role: LlmRole::User,
        content: vec![LlmContentBlock::Text {
            text: format!(
                "Directories to explore: {}\nFiles the span touched: {}\n\nThe span:\n{}",
                dirs.join(", "),
                paths
                    .iter()
                    .take(40)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                render_span(span)
            ),
        }],
    }];

    for round in 0..=MAX_ROUNDS {
        let last = round == MAX_ROUNDS;
        let res = llm
            .run(
                LlmParams {
                    model: model.clone(),
                    system: Some(SYSTEM.to_string()),
                    system_volatile: None,
                    max_tokens: MAX_TOKENS,
                    messages: messages.clone(),
                    tools: if last { vec![] } else { vec![bash_tool()] },
                    // The write-up round. Without this the cap would throw away
                    // everything the scout learned in the rounds it did get.
                    tool_choice_none: last,
                    effort: None,
                },
                Arc::new(|_| {}),
                cancel.clone(),
            )
            .await
            .ok()?;

        let calls: Vec<(String, serde_json::Value)> = res
            .content
            .iter()
            .filter_map(|b| match b {
                LlmBlock::ToolUse { id, input, .. } => Some((id.clone(), input.clone())),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            let text = res
                .content
                .iter()
                .filter_map(|b| match b {
                    LlmBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
                .trim()
                .to_string();
            return if text.is_empty() { None } else { Some(text) };
        }

        messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: res
                .content
                .iter()
                .cloned()
                .map(LlmContentBlock::from)
                .collect(),
        });
        let mut results: Vec<LlmContentBlock> = Vec::with_capacity(calls.len());
        for (id, input) in calls {
            let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if command.trim().is_empty() {
                results.push(LlmContentBlock::ToolResult {
                    tool_use_id: id,
                    content: "bash needs a non-empty command string".to_string(),
                    is_error: true,
                });
                continue;
            }
            // Tagged like any other command in the session, because it IS one:
            // it lands in the tag history where a later session can see what
            // the scout looked at.
            let out = match shell.bash(command, Some("compact:explore:scout")).await {
                Ok(out) => out,
                Err(err) => format!("[failed] {err}"),
            };
            let content = if out.chars().count() > OUTPUT_CLIP {
                format!("{}…", out.chars().take(OUTPUT_CLIP).collect::<String>())
            } else {
                out
            };
            results.push(LlmContentBlock::ToolResult {
                tool_use_id: id,
                content,
                is_error: false,
            });
        }
        messages.push(LlmMessage {
            role: LlmRole::User,
            content: results,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/explore.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BoughError;
    use crate::schema::parts::{Part, Role};
    use crate::types::{LlmResult, OnText};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct TmpWorkspace(PathBuf);
    impl TmpWorkspace {
        fn new() -> TmpWorkspace {
            let dir = std::env::temp_dir().join(format!("bough-explore-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("src/history")).unwrap();
            std::fs::write(dir.join("src/history/compact.ts"), "export const x = 1;\n").unwrap();
            std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
            TmpWorkspace(dir)
        }
        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }
    impl Drop for TmpWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn span(texts: &[&str]) -> Vec<Message> {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| Message {
                id: format!("m{i}"),
                session_id: "s1".into(),
                role: Role::Supervisor,
                parts: vec![Part::Text {
                    text: (*text).to_string(),
                }],
                pending: false,
                created_at: 1_000 + i as i64,
            })
            .collect()
    }

    // ---- what it is pointed at ----------------------------------------------

    #[test]
    fn touched_paths_keeps_what_exists_and_drops_what_only_looks_real() {
        let ws = TmpWorkspace::new();
        let paths = touched_paths(
            &span(&[
                "I edited src/history/compact.ts and README.md",
                "then src/history/nosuch.ts, and bumped to v1.2.4 — see docs/plan.md",
            ]),
            &ws.path(),
        );
        assert!(
            paths.contains(&"src/history/compact.ts".to_string()),
            "{paths:?}"
        );
        assert!(paths.contains(&"README.md".to_string()), "{paths:?}");
        // Named in the transcript, absent from the tree: nothing to explore.
        assert!(!paths.contains(&"src/history/nosuch.ts".to_string()));
        assert!(!paths.contains(&"docs/plan.md".to_string()));
        // The version number is exactly the kind of token the loose regex
        // matches and the filesystem throws away.
        assert!(!paths.iter().any(|p| p.contains("1.2.4")));
    }

    #[test]
    fn touched_paths_refuses_a_real_path_outside_the_workspace() {
        let ws = TmpWorkspace::new();
        let outside = TmpWorkspace::new();
        std::fs::write(outside.0.join("elsewhere.txt"), "x").unwrap();
        let mentioned = outside
            .0
            .join("elsewhere.txt")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            touched_paths(&span(&[&format!("I also read {mentioned}")]), &ws.path()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn touched_dirs_is_the_directories_deduped_and_in_order() {
        assert_eq!(
            touched_dirs(&[
                "src/history/compact.ts".to_string(),
                "src/history/branch.ts".to_string(),
                "README.md".to_string(),
            ]),
            vec!["src/history".to_string(), ".".to_string()]
        );
    }

    // ---- the loop -----------------------------------------------------------

    struct ScoutLlm {
        round: Mutex<usize>,
        commands: Mutex<Vec<String>>,
        briefs: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LlmClient for ScoutLlm {
        async fn run(
            &self,
            params: LlmParams,
            _on_text: OnText,
            _cancel: CancellationToken,
        ) -> Result<LlmResult, BoughError> {
            let round = {
                let mut r = self.round.lock().unwrap();
                *r += 1;
                *r
            };
            if round == 1 {
                if let Some(LlmContentBlock::Text { text }) = params.messages[0].content.first() {
                    self.briefs.lock().unwrap().push(text.clone());
                }
                return Ok(LlmResult {
                    content: vec![LlmBlock::ToolUse {
                        id: "t1".into(),
                        name: "bash".into(),
                        input: json!({ "command": "echo SCOUTED" }),
                    }],
                    stop_reason: "tool_use".into(),
                    usage: None,
                });
            }
            // The tool result must have come back as a user message, or the
            // scout is reasoning about a command it never saw the output of.
            let last = params.messages.last().expect("a round always has messages");
            match last.content.first() {
                Some(LlmContentBlock::ToolResult { content, .. }) => {
                    self.commands
                        .lock()
                        .unwrap()
                        .push(content.trim().to_string());
                }
                other => panic!("the command's output never reached the scout: {other:?}"),
            }
            Ok(LlmResult {
                content: vec![LlmBlock::Text {
                    text: "notes: the file is there".into(),
                }],
                stop_reason: "end_turn".into(),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn the_scout_runs_its_bash_calls_and_returns_the_notes_it_ends_with() {
        let ws = TmpWorkspace::new();
        let llm = Arc::new(ScoutLlm {
            round: Mutex::new(0),
            commands: Mutex::new(vec![]),
            briefs: Mutex::new(vec![]),
        });
        let notes = explore_span(
            &ExploreCtx {
                session_id: "s1".into(),
                workspace: ws.path(),
                llm: Some(llm.clone()),
                model: Some("test-model".into()),
                registry: None,
            },
            &span(&["I edited src/history/compact.ts"]),
        )
        .await;

        assert_eq!(notes.as_deref(), Some("notes: the file is there"));
        assert_eq!(
            llm.commands.lock().unwrap().clone(),
            vec!["SCOUTED".to_string()]
        );
        // The brief must name the directory the span's files live in — that is
        // the scoping this whole module exists for.
        assert!(
            llm.briefs.lock().unwrap()[0].contains("src/history"),
            "no scope in brief"
        );
    }

    struct NeverLlm;
    #[async_trait]
    impl LlmClient for NeverLlm {
        async fn run(
            &self,
            _params: LlmParams,
            _on_text: OnText,
            _cancel: CancellationToken,
        ) -> Result<LlmResult, BoughError> {
            panic!("the scout must not run with no paths");
        }
    }

    #[tokio::test]
    async fn a_span_that_touched_nothing_that_exists_is_not_scouted_at_all() {
        let ws = TmpWorkspace::new();
        assert_eq!(
            explore_span(
                &ExploreCtx {
                    session_id: "s1".into(),
                    workspace: ws.path(),
                    llm: Some(Arc::new(NeverLlm)),
                    model: Some("m".into()),
                    registry: None,
                },
                &span(&["we talked"]),
            )
            .await,
            None
        );
    }

    struct ThrowingLlm;
    #[async_trait]
    impl LlmClient for ThrowingLlm {
        async fn run(
            &self,
            _params: LlmParams,
            _on_text: OnText,
            _cancel: CancellationToken,
        ) -> Result<LlmResult, BoughError> {
            Err(BoughError::llm("401 no key for that provider"))
        }
    }

    #[tokio::test]
    async fn a_scout_that_fails_yields_none_never_an_error() {
        let ws = TmpWorkspace::new();
        assert_eq!(
            explore_span(
                &ExploreCtx {
                    session_id: "s1".into(),
                    workspace: ws.path(),
                    llm: Some(Arc::new(ThrowingLlm)),
                    model: Some("m".into()),
                    registry: None,
                },
                &span(&["I edited src/history/compact.ts"]),
            )
            .await,
            None
        );
    }

    #[test]
    fn the_scout_model_is_pinned_and_overridable_for_a_user_with_a_different_key() {
        assert_eq!(explore_model_from(&|_| None), "gpt-5.6-luna");
        assert_eq!(
            explore_model_from(
                &|k| (k == "BOUGH_COMPACT_EXPLORE_MODEL").then(|| " claude-opus-4-8 ".to_string())
            ),
            "claude-opus-4-8"
        );
        // An empty override reads as unset, not as an empty model id.
        assert_eq!(
            explore_model_from(&|_| Some("  ".to_string())),
            "gpt-5.6-luna"
        );
    }
}
