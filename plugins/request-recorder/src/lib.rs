//! Invariant: EVERY request that reaches a model is on disk, verbatim, before the model sees it —
//! `$BOUGH_HOME/<dir>/<digest>.md`, one file per projection (the digest the agent loop writes as
//! `request/header.projection_digest`, carried on the request; the system text's own sha256 when
//! no projection built the request), one `## Round` section per request sent under it: the stable
//! system text and the tool list once, then each round's volatile tier and messages exactly as
//! sent. The conversation's click on a speaker label opens the turn's file in `$EDITOR` (Andrey,
//! 2026-08-28: "the entire context that was sent as a turn").
//!
//! Listens on the `llm/stream` waterfall and never touches the call: it writes, then `next`.
//! Zero edits to the agent loop or to `bough-llm`; disabling the row stops the recording and
//! nothing else changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{Context, Inject, Plugin, PluginError};
use bough_llm::LlmRole;
use bough_plugin_llm::request::LlmRequest;
use bough_plugin_llm::stream::{LlmStreamEvent, StreamCall};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "request-recorder";

/// The bundle's `dir`, and what the conversation's click looks under.
pub const DEFAULT_DIR: &str = "requests";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecorderConfig {
    /// Where the files go, relative to `$BOUGH_HOME` (an absolute path is taken as is).
    #[serde(default = "default_dir")]
    pub dir: String,
    /// How many columns of a message to keep before `…` (0 = everything).
    #[serde(default)]
    pub max_block_chars: usize,
}

fn default_dir() -> String {
    DEFAULT_DIR.to_string()
}

impl Default for RecorderConfig {
    fn default() -> Self {
        RecorderConfig {
            dir: default_dir(),
            max_block_chars: 0,
        }
    }
}

/// The row.
pub struct RequestRecorderPlugin;

#[async_trait::async_trait]
impl Plugin for RequestRecorderPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = RecorderConfig;

    fn inject() -> Inject {
        Inject::required(["llm"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let base = resolve_dir(&cfg.dir);
        ctx.on_waterfall::<LlmStreamEvent, _, _>(move |call: StreamCall, next| {
            let base = base.clone();
            let cfg = cfg.clone();
            async move {
                let request = call.request.clone();
                let when = chrono::Utc::now();
                // Off the async path: a file append is fast, but the model call must not wait
                // on the disk in the loop's own task.
                let _ = tokio_free_write(&base, &request, when, cfg.max_block_chars);
                next.run(call).await
            }
        })
        .await?;
        Ok(())
    }
}

/// `dir` under `$BOUGH_HOME`, or as is when absolute.
pub fn resolve_dir(dir: &str) -> PathBuf {
    let p = Path::new(dir);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        bough_util::bough_home().join(p)
    }
}

fn tokio_free_write(
    base: &Path,
    request: &LlmRequest,
    when: chrono::DateTime<chrono::Utc>,
    max_block_chars: usize,
) -> std::io::Result<PathBuf> {
    let path = file_for(base, request);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let round = existing.matches("\n## Round ").count() + 1;
    let mut out = String::new();
    if existing.is_empty() {
        out.push_str(&head(request));
    }
    out.push_str(&round_section(request, round, when, max_block_chars));
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(out.as_bytes())?;
    tracing::debug!(target: "request-recorder", path = %path.display(), round, "recorded");
    Ok(path)
}

/// PURE: sha256 of the system text, hex — the agent loop's `projection_digest`, so the
/// conversation can find the file from the turn's `request/header` step.
pub fn digest(system: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(system.as_bytes());
    format!("{:x}", h.finalize())
}

/// PURE: the file a request lands in: `<base>/<digest>.md`.
///
/// The request CARRIES the header's `projection_digest` (over the whole projection, both tiers),
/// so the conversation's lookup by the `request/header` step finds this file; a request built
/// with no projection (governance, the summarizer) falls back to the digest of its own system.
pub fn file_for(base: &Path, request: &LlmRequest) -> PathBuf {
    let key = match &request.projection_digest {
        Some(d) if !d.is_empty() => d.clone(),
        _ => digest(request.system.as_deref().unwrap_or("")),
    };
    path_for_digest(base, &key)
}

/// PURE: the file for a `projection_digest` a `request/header` step carries — the conversation's
/// side of the same rule.
pub fn path_for_digest(base: &Path, digest: &str) -> PathBuf {
    base.join(format!("{digest}.md"))
}

/// The file's head, written once: the digest, the system text and the tools.
fn head(request: &LlmRequest) -> String {
    let system = request.system.as_deref().unwrap_or("");
    let mut s = String::new();
    s.push_str(&format!(
        "# Request context · projection {}\n\n",
        &digest(system)[..12]
    ));
    s.push_str("Every request sent to the model under this projection, verbatim: the system text and the tool list once, then each round's messages exactly as sent.\n\n");
    s.push_str("## System\n\n");
    if system.is_empty() {
        s.push_str("_(no system text)_\n\n");
    } else {
        s.push_str(system);
        if !system.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str("## Tools\n\n");
    if request.tools.is_empty() {
        s.push_str("_(none)_\n\n");
    }
    for t in &request.tools {
        s.push_str(&format!(
            "- `{}` — {}\n",
            t.name,
            first_line(&t.description)
        ));
    }
    s.push('\n');
    s
}

/// One round: the call config and the messages as sent.
fn round_section(
    request: &LlmRequest,
    round: usize,
    when: chrono::DateTime<chrono::Utc>,
    max_block_chars: usize,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "\n## Round {round} · {} · {}\n\n",
        request.model,
        when.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    s.push_str(&format!(
        "max_tokens {} · effort {} · tool_choice_none {}\n\n",
        request.call.max_tokens,
        request
            .call
            .effort
            .as_ref()
            .map(|e| format!("{e:?}").to_lowercase())
            .unwrap_or_else(|| "default".to_string()),
        request.call.tool_choice_none
    ));
    // PER ROUND, not in the head: the volatile tier (the tail band and mail) is exactly the part
    // of the projection that moves between requests under one stable prefix.
    if let Some(v) = request.system_volatile.as_deref() {
        s.push_str("### System (volatile)\n\n");
        s.push_str(v);
        if !v.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
    }
    for (i, m) in request.messages.iter().enumerate() {
        let role = match m.role {
            LlmRole::User => "user",
            LlmRole::Assistant => "assistant",
        };
        s.push_str(&format!("### {} · {role}\n\n", i + 1));
        for block in &m.content {
            s.push_str(&render_block(block, max_block_chars));
        }
    }
    s
}

/// A content block as markdown: text as prose, everything else as fenced JSON.
fn render_block(block: &bough_llm::LlmContentBlock, max_block_chars: usize) -> String {
    use bough_llm::LlmContentBlock;
    let clip = |t: &str| -> String {
        if max_block_chars > 0 && t.chars().count() > max_block_chars {
            t.chars().take(max_block_chars).collect::<String>() + "\u{2026}"
        } else {
            t.to_string()
        }
    };
    match block {
        LlmContentBlock::Text { text } => format!("{}\n\n", clip(text)),
        other => {
            let json = serde_json::to_string_pretty(other).unwrap_or_default();
            format!("```json\n{}\n```\n\n", clip(&json))
        }
    }
}

fn first_line(t: &str) -> &str {
    t.lines().next().unwrap_or("").trim()
}

bough_kernel::register_plugin!(RequestRecorderPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use bough_llm::{LlmContentBlock, LlmMessage, LlmToolDef};
    use bough_plugin_llm::request::CallConfig;
    use chrono::TimeZone;

    fn request(system: &str, n: usize) -> LlmRequest {
        LlmRequest {
            projection_digest: None,
            model: "claude-haiku-4-5".into(),
            system: Some(system.to_string()),
            system_volatile: None,
            messages: (0..n)
                .map(|i| LlmMessage {
                    role: if i % 2 == 0 {
                        LlmRole::User
                    } else {
                        LlmRole::Assistant
                    },
                    content: vec![LlmContentBlock::Text {
                        text: format!("message {i}"),
                    }],
                })
                .collect(),
            tools: vec![LlmToolDef {
                name: "run".into(),
                description: "Run a program.\nMore.".into(),
                input_schema: serde_json::json!({}),
            }],
            call: CallConfig {
                model: "claude-haiku-4-5".into(),
                max_tokens: 4096,
                effort: None,
                tool_choice_none: false,
                meta: Default::default(),
            },
        }
    }

    #[test]
    fn the_file_is_keyed_by_the_projection_digest_and_grows_a_round_per_request() {
        let dir = std::env::temp_dir().join(format!("bough-recorder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let when = chrono::Utc.with_ymd_and_hms(2026, 8, 28, 17, 0, 0).unwrap();
        let r1 = request("You are sol.", 1);
        let p1 = tokio_free_write(&dir, &r1, when, 0).expect("written");
        assert_eq!(p1, dir.join(format!("{}.md", digest("You are sol."))));
        let text = std::fs::read_to_string(&p1).unwrap();
        assert!(
            text.starts_with("# Request context · projection "),
            "{text}"
        );
        assert!(text.contains("## System\n\nYou are sol.\n"), "{text}");
        assert!(text.contains("- `run` — Run a program."), "{text}");
        assert!(
            text.contains("## Round 1 · claude-haiku-4-5 · 2026-08-28 17:00:00 UTC"),
            "{text}"
        );
        assert!(text.contains("### 1 · user\n\nmessage 0\n"), "{text}");
        // The next request under the same projection is round 2 of the same file; the system
        // text is not repeated.
        let r2 = request("You are sol.", 3);
        let p2 = tokio_free_write(&dir, &r2, when, 0).expect("written");
        assert_eq!(p1, p2);
        let text = std::fs::read_to_string(&p1).unwrap();
        assert_eq!(text.matches("## System").count(), 1);
        assert!(text.contains("## Round 2 ·"), "{text}");
        assert!(text.contains("### 3 · user\n\nmessage 2\n"), "{text}");
        // A different projection is a different file.
        let r3 = request("You are terra.", 1);
        assert_ne!(file_for(&dir, &r3), p1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_carried_projection_digest_keys_the_file_and_the_volatile_tier_lands_per_round() {
        let dir = std::env::temp_dir().join(format!("bough-recorder-v-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let when = chrono::Utc.with_ymd_and_hms(2026, 8, 29, 9, 0, 0).unwrap();
        // Two requests under ONE stable prefix whose volatile tails differ: the header's digest
        // (carried on the request) is the key, so both rounds land in the header's file even
        // though digest(system) alone would disagree with it.
        let mut r1 = request("You are sol.", 1);
        r1.projection_digest = Some("feedc0de".into());
        r1.system_volatile = Some("## Recent steps\n\nandrey: hi".into());
        let mut r2 = request("You are sol.", 3);
        r2.projection_digest = Some("feedc0de".into());
        r2.system_volatile = Some("## Recent steps\n\nandrey: hi\nsol: answered".into());
        let p1 = tokio_free_write(&dir, &r1, when, 0).expect("written");
        assert_eq!(p1, dir.join("feedc0de.md"));
        let p2 = tokio_free_write(&dir, &r2, when, 0).expect("written");
        assert_eq!(p1, p2);
        let text = std::fs::read_to_string(&p1).unwrap();
        assert_eq!(
            text.matches("### System (volatile)").count(),
            2,
            "the volatile tier is per ROUND: it is the part that moves\n{text}"
        );
        assert!(text.contains("sol: answered"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_text_block_is_fenced_json_and_a_long_block_is_clipped() {
        let block = LlmContentBlock::Text {
            text: "x".repeat(50),
        };
        let out = render_block(&block, 10);
        assert_eq!(out, format!("{}\u{2026}\n\n", "x".repeat(10)));
        let tool = serde_json::from_value::<LlmContentBlock>(serde_json::json!({
            "type": "tool_use", "id": "c1", "name": "run", "input": { "program": "1+1" }
        }));
        if let Ok(tool) = tool {
            let out = render_block(&tool, 0);
            assert!(out.starts_with("```json\n"), "{out}");
            assert!(out.contains("\"run\""), "{out}");
        }
    }
}
