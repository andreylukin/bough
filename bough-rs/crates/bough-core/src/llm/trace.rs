//! Raw provider I/O, on disk, for harness experiments (port of
//! `src/llm/trace.ts`).
//!
//! WHAT THIS IS FOR. Nothing else in the tree records what was actually SENT
//! to the model. `messages` stores rendered parts — the turn as the UI shows
//! it — which is the right store for a conversation and the wrong one for an
//! experiment: it cannot answer "which system prompt bytes produced this
//! round". So this decorator writes the request and the response verbatim,
//! per round, including the rounds that FAILED — an error is evidence too,
//! and the retry wrapper would otherwise swallow it.
//!
//! OFF UNLESS ASKED. No `BOUGH_TRACE_DIR`, no sink, no cost: `trace_label`
//! answers `None` and `with_trace` hands back the inner client unwrapped.
//!
//! THE FORMAT is JSONL, one file per turn, one line per round, self-contained:
//!
//! ```jsonc
//! {"type":"prompt","tier":"system","sha":"…","text":"…"}   // first sight
//! {"type":"round","n":1,"systemSha":"…","request":{…},"response":{…}}
//! ```
//!
//! A prefix is written ONCE and referenced by sha afterwards, because it is
//! byte-identical across every round of a turn and repeating 30KB per round
//! would bury the signal. The file still reconstructs standalone.
//!
//! Composition order (load-bearing, `client_for`): trace sits INSIDE the
//! retries so a recorded trace shows each attempt — outside them it would
//! collapse five failed attempts into the sixth's success — and outside
//! pricing so a recorded round already carries its cost.

use std::collections::HashSet;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::llm::routing::Env;
use crate::prompt::assemble::{section_sha, SectionSha};
use crate::types::{LlmClient, LlmParams, LlmResult, OnText};

/// Where one turn's raw provider I/O goes.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceLabel {
    pub dir: String,
    pub session_id: String,
    pub turn_id: String,
}

/// `BOUGH_TRACE_DIR` (trimmed) set → a label; unset or blank → `None` — off
/// unless asked, no sink, no cost.
pub fn trace_label(session_id: &str, turn_id: &str, env: &Env) -> Option<TraceLabel> {
    let dir = env("BOUGH_TRACE_DIR")?;
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    Some(TraceLabel {
        dir: dir.to_string(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
    })
}

/// The path a label's rounds are appended to. One file per turn keyed by both
/// ids: concurrent turns write concurrently, and a single shared file would
/// interleave their rounds into nonsense.
pub fn trace_path(label: &TraceLabel) -> PathBuf {
    Path::new(&label.dir)
        .join(&label.session_id)
        .join(format!("{}.jsonl", label.turn_id))
}

/// The path a label's manifest is written to.
pub fn manifest_path(label: &TraceLabel) -> PathBuf {
    Path::new(&label.dir)
        .join(&label.session_id)
        .join(format!("{}.manifest.json", label.turn_id))
}

/// What the turn knew that the provider boundary does not: which prompt
/// sections went in, and what the turn was configured with.
///
/// `LlmParams` carries the assembled prefix as one opaque string, so section
/// identity has to be written from where assembly happened. That is the whole
/// point of the manifest — an editable component is a SECTION, and without
/// this the trace can only say "the prefix changed", never which file did.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnManifest {
    pub session_id: String,
    pub turn_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Every included section, in prompt order, with the sha of its text.
    pub sections: Vec<SectionSha>,
    pub started_at: i64,
}

/// Append one JSON line. **Every filesystem error is swallowed**: a trace is
/// diagnostic. A full disk or an unwritable directory must never be the
/// reason a turn dies, and there is no one to tell — the sink has no channel
/// to the user by design.
fn write_line(path: &Path, value: &Value) {
    let Some(parent) = path.parent() else { return };
    if create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{value}");
}

/// Write a turn's manifest, pretty-printed. Called once, from where the
/// prompt was assembled. As above: diagnostic, never fatal.
pub fn write_manifest(label: &TraceLabel, manifest: &TurnManifest) {
    let path = manifest_path(label);
    let Some(parent) = path.parent() else { return };
    if create_dir_all(parent).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string_pretty(manifest) else {
        return;
    };
    let _ = std::fs::write(&path, format!("{text}\n"));
}

/// A round as it lands in the JSONL. Public so a reader can type what it
/// parses. `response` and `error` are a true XOR — both are omitted when
/// absent, never written as null.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoundRecord {
    #[serde(rename = "type")]
    pub kind: String,
    /// 1-based within this turn, counting failed attempts.
    pub n: u32,
    pub ts: i64,
    pub latency_ms: i64,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub system_sha: String,
    pub volatile_sha: String,
    pub request: RoundRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Present instead of `response` when the attempt threw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RoundError>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoundRequest {
    pub max_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Tool NAMES only: the schemas are fixed per build and identical every
    /// round.
    pub tools: Vec<String>,
    pub messages: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoundError {
    pub name: String,
    pub message: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct TracingClient {
    inner: Arc<dyn LlmClient>,
    path: PathBuf,
    /// Prefix shas already written this turn.
    seen: Mutex<HashSet<String>>,
    /// 1-based round counter, monotonic per wrapped client, counting FAILED
    /// attempts — the retry ring sits outside this decorator.
    n: Mutex<u32>,
}

impl TracingClient {
    /// Emit a prefix's text the first time this turn sends it; return its sha
    /// either way.
    fn prefix(&self, tier: &str, text: &str) -> String {
        let sha = section_sha(text);
        let fresh = self.seen.lock().unwrap().insert(sha.clone());
        if fresh {
            write_line(
                &self.path,
                &json!({ "type": "prompt", "tier": tier, "sha": sha, "text": text }),
            );
        }
        sha
    }
}

#[async_trait::async_trait]
impl LlmClient for TracingClient {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        let round = {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            *n
        };
        let system_sha = self.prefix("system", params.system.as_deref().unwrap_or(""));
        let volatile_sha = self.prefix("volatile", params.system_volatile.as_deref().unwrap_or(""));
        let started = now_ms();
        let base = RoundRecord {
            kind: "round".into(),
            n: round,
            ts: started,
            latency_ms: 0,
            model: params.model.clone(),
            effort: params
                .effort
                .and_then(|e| serde_json::to_value(e).ok())
                .and_then(|v| v.as_str().map(String::from)),
            system_sha,
            volatile_sha,
            request: RoundRequest {
                max_tokens: params.max_tokens,
                tool_choice: params.tool_choice_none.then(|| "none".to_string()),
                tools: params.tools.iter().map(|t| t.name.clone()).collect(),
                messages: serde_json::to_value(&params.messages).unwrap_or(Value::Null),
            },
            response: None,
            error: None,
        };

        match self.inner.run(params, on_text, cancel).await {
            Ok(result) => {
                let record = RoundRecord {
                    latency_ms: now_ms() - started,
                    response: Some(json!({
                        "content": result.content,
                        "stopReason": result.stop_reason,
                        "usage": result.usage,
                    })),
                    ..base
                };
                if let Ok(value) = serde_json::to_value(&record) {
                    write_line(&self.path, &value);
                }
                Ok(result)
            }
            Err(err) => {
                let record = RoundRecord {
                    latency_ms: now_ms() - started,
                    error: Some(RoundError {
                        name: err.name().to_string(),
                        message: err.to_string(),
                    }),
                    ..base
                };
                if let Ok(value) = serde_json::to_value(&record) {
                    write_line(&self.path, &value);
                }
                Err(err)
            }
        }
    }
}

/// Record every round this client runs. Returns `inner` **identity-untouched**
/// when `label` is `None` (test-pinned), so the non-tracing path pays nothing.
pub fn with_trace(inner: Arc<dyn LlmClient>, label: Option<TraceLabel>) -> Arc<dyn LlmClient> {
    let Some(label) = label else { return inner };
    Arc::new(TracingClient {
        path: trace_path(&label),
        inner,
        seen: Mutex::new(HashSet::new()),
        n: Mutex::new(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::fake_client;
    use crate::schema::parts::Usage;
    use crate::types::{LlmBlock, LlmContentBlock, LlmMessage, LlmRole, LlmToolDef};
    use serde_json::Map;

    fn label() -> TraceLabel {
        let dir = std::env::temp_dir().join(format!("bough-trace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdtemp");
        TraceLabel {
            dir: dir.to_string_lossy().into_owned(),
            session_id: "s1".into(),
            turn_id: "t1".into(),
        }
    }

    fn params() -> LlmParams {
        LlmParams {
            model: "claude-haiku-4-5".into(),
            system: Some("STABLE PREFIX".into()),
            system_volatile: Some("VOLATILE".into()),
            max_tokens: 100,
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text { text: "hi".into() }],
            }],
            tools: vec![LlmToolDef {
                name: "run_steps".into(),
                description: "d".into(),
                input_schema: Value::Object(Map::new()),
            }],
            tool_choice_none: false,
            effort: None,
        }
    }

    fn result() -> LlmResult {
        LlmResult {
            content: vec![LlmBlock::Text { text: "ok".into() }],
            stop_reason: "end_turn".into(),
            usage: Some(Usage {
                input_tokens: 1,
                output_tokens: 2,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: Some(0.5),
            }),
        }
    }

    fn lines(label: &TraceLabel) -> Vec<Value> {
        std::fs::read_to_string(trace_path(label))
            .unwrap()
            .trim()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_round_records_the_request_the_response_and_the_prefix_it_ran_with() {
        let l = label();
        let (inner, _) = fake_client(vec![Ok(result())]);
        with_trace(inner, Some(l.clone()))
            .run(params(), Arc::new(|_| {}), CancellationToken::new())
            .await
            .unwrap();

        let all = lines(&l);
        assert_eq!(all[0]["tier"], "system");
        assert_eq!(all[0]["text"], "STABLE PREFIX");
        assert_eq!(all[1]["tier"], "volatile");
        assert_eq!(all[1]["text"], "VOLATILE");
        let round = &all[2];
        // The fact the whole loop rests on: the prefix a round ran with is
        // recoverable from the file, byte for byte, not merely named by it.
        assert_eq!(round["systemSha"], section_sha("STABLE PREFIX"));
        assert_eq!(round["volatileSha"], section_sha("VOLATILE"));
        assert_eq!(round["n"], 1);
        assert_eq!(round["model"], "claude-haiku-4-5");
        assert_eq!(round["request"]["tools"], json!(["run_steps"]));
        assert_eq!(
            round["request"]["messages"],
            serde_json::to_value(params().messages).unwrap()
        );
        assert_eq!(round["response"]["stopReason"], "end_turn");
        // Cost is present because tracing wraps pricing, not the other way round.
        assert_eq!(round["response"]["usage"]["costUsd"], 0.5);
        assert!(round["latencyMs"].as_i64().unwrap() >= 0);
        // toolChoice is omitted, not null, when the round did not set it.
        assert!(round["request"].get("toolChoice").is_none());
    }

    #[tokio::test]
    async fn an_unchanged_prefix_is_written_once_and_referenced_by_sha_afterwards() {
        let l = label();
        let (inner, _) = fake_client(vec![Ok(result()), Ok(result())]);
        let client = with_trace(inner, Some(l.clone()));
        for _ in 0..2 {
            client
                .run(params(), Arc::new(|_| {}), CancellationToken::new())
                .await
                .unwrap();
        }
        let all = lines(&l);
        assert_eq!(
            all.iter().filter(|r| r["type"] == "prompt").count(),
            2,
            "one per tier, not per round"
        );
        let rounds: Vec<&Value> = all.iter().filter(|r| r["type"] == "round").collect();
        assert_eq!(
            rounds
                .iter()
                .map(|r| r["n"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(rounds[0]["systemSha"], rounds[1]["systemSha"]);
    }

    #[tokio::test]
    async fn a_failed_attempt_is_recorded_and_still_throws() {
        let l = label();
        let (inner, _) = fake_client(vec![Err(BoughError::llm("boom"))]);
        let err = with_trace(inner, Some(l.clone()))
            .run(params(), Arc::new(|_| {}), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
        // The retry wrapper sits OUTSIDE this one precisely so a swallowed
        // attempt is still evidence: an experiment reading only successes
        // would misread a flaky round as a clean one.
        let round = lines(&l)
            .into_iter()
            .find(|r| r["type"] == "round")
            .unwrap();
        assert_eq!(round["error"]["message"], "boom");
        assert_eq!(round["error"]["name"], "LlmError");
        assert!(
            round.get("response").is_none(),
            "response and error are a XOR"
        );
    }

    #[tokio::test]
    async fn n_counts_failed_attempts_not_successes() {
        let l = label();
        let (inner, _) = fake_client(vec![
            Err(BoughError::llm("first")),
            Err(BoughError::llm("second")),
            Ok(result()),
        ]);
        let client = with_trace(inner, Some(l.clone()));
        for _ in 0..3 {
            let _ = client
                .run(params(), Arc::new(|_| {}), CancellationToken::new())
                .await;
        }
        let rounds: Vec<Value> = lines(&l)
            .into_iter()
            .filter(|r| r["type"] == "round")
            .collect();
        assert_eq!(
            rounds
                .iter()
                .map(|r| r["n"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "n is the attempt number, not the success number"
        );
        assert!(rounds[0]["error"].is_object());
        assert!(rounds[1]["error"].is_object());
        assert!(rounds[2]["response"].is_object());
    }

    #[tokio::test]
    async fn an_unwritable_directory_never_kills_the_turn() {
        // All fs errors are swallowed: the label's dir is a FILE, so every
        // create_dir_all under it fails.
        let blocker =
            std::env::temp_dir().join(format!("bough-trace-blk-{}", uuid::Uuid::new_v4()));
        std::fs::write(&blocker, "not a directory").unwrap();
        let l = TraceLabel {
            dir: blocker.to_string_lossy().into_owned(),
            session_id: "s1".into(),
            turn_id: "t1".into(),
        };
        let (inner, _) = fake_client(vec![Ok(result())]);
        let out = with_trace(inner, Some(l.clone()))
            .run(params(), Arc::new(|_| {}), CancellationToken::new())
            .await;
        assert!(out.is_ok(), "a dead sink must not fail the round");
        write_manifest(
            &l,
            &TurnManifest {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                model: "m".into(),
                effort: None,
                workspace: None,
                sections: vec![],
                started_at: 0,
            },
        );
        assert!(!manifest_path(&l).exists());
        let _ = std::fs::remove_file(&blocker);
    }

    #[tokio::test]
    async fn the_manifest_carries_the_section_identities_the_raw_trace_cannot_see() {
        let l = label();
        let manifest = TurnManifest {
            session_id: "s1".into(),
            turn_id: "t1".into(),
            model: "claude-opus-5".into(),
            effort: Some("high".into()),
            workspace: Some("/w".into()),
            sections: vec![SectionSha {
                id: crate::prompt::assemble::SectionId::Identity,
                sha: section_sha("x"),
            }],
            started_at: 1234,
        };
        write_manifest(&l, &manifest);
        let text = std::fs::read_to_string(manifest_path(&l)).unwrap();
        let parsed: TurnManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, manifest);
        assert!(text.ends_with('\n'));
        assert!(text.contains("\n  "), "pretty-printed for a human reader");
    }

    #[test]
    fn tracing_off_returns_the_client_untouched() {
        let (inner, _calls) = fake_client(vec![]);
        let wrapped = with_trace(inner.clone(), None);
        assert!(
            Arc::ptr_eq(&inner, &wrapped),
            "with_trace(inner, None) must be the identity"
        );
    }

    #[test]
    fn trace_label_reads_bough_trace_dir_trimmed() {
        let none: Env = Arc::new(|_| None);
        assert_eq!(trace_label("s", "t", &none), None);
        let blank: Env = Arc::new(|_| Some("  ".into()));
        assert_eq!(
            trace_label("s", "t", &blank),
            None,
            "a blank dir is not a directory"
        );
        let set: Env = Arc::new(|k| (k == "BOUGH_TRACE_DIR").then(|| "/tmp/x".to_string()));
        assert_eq!(
            trace_label("s", "t", &set),
            Some(TraceLabel {
                dir: "/tmp/x".into(),
                session_id: "s".into(),
                turn_id: "t".into()
            })
        );
        let l = TraceLabel {
            dir: "/tmp/x".into(),
            session_id: "s".into(),
            turn_id: "t".into(),
        };
        assert_eq!(trace_path(&l), Path::new("/tmp/x/s/t.jsonl"));
        assert_eq!(manifest_path(&l), Path::new("/tmp/x/s/t.manifest.json"));
    }
}
