//! Invariant: `sh` runs its legs CONCURRENTLY and never throws on a non-zero exit — the exit code
//! is data, one `{code, out}` per leg, in the order the legs were given. A leg that carries no
//! tags is refused before anything runs: a command recorded with no tags is one no future session
//! can find, and that rule belongs to the tool, not to whichever surface called it.
//!
//! MERGE (`docs/codemode-merge-notes.md` §9, "Still open"): `surface/shell.md` documented `sh()`
//! while the tree registered no such tool anywhere, so the sandbox taught the model a function it
//! could not call. This is that Provider, and it lives beside `bg` because the note says so:
//! `tools-baseline` owns the ONE serial shell, this row owns the concurrent one and the
//! background one.

use std::path::PathBuf;
use std::sync::Arc;

use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};
use tokio_util::sync::CancellationToken;

use crate::OperatorConfig;

/// One leg of a concurrent shell call.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Leg {
    pub cmd: String,
    /// 3-5 short words naming the tool, the intent and the subject. `tag` is accepted as a
    /// spelling of the same field so a colon-separated string (`surface/shell.md`'s older form)
    /// still parses; the ARRAY is what the schema declares and what the prose now teaches.
    #[serde(default, alias = "tag")]
    pub tags: serde_json::Value,
}

/// What one leg produced. Serialised as-is: this IS the shape `surface/shell.md` promises.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct LegResult {
    pub code: i32,
    pub out: String,
}

pub struct Sh {
    pub cfg: Arc<OperatorConfig>,
    /// The pinned workspace root: every leg starts there, exactly as `bash` does.
    pub root: PathBuf,
}

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

/// The tags one leg carried, in either spelling.
///
/// Written here rather than reached for across a crate boundary: `tools-codemode` has the same
/// reader for its own surface and neither crate depends on the other.
pub fn leg_tags(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => s
            .split(':')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect(),
        serde_json::Value::Array(a) => a.iter().flat_map(leg_tags).collect(),
        _ => Vec::new(),
    }
}

/// Parse and check the legs. PURE, so every refusal wording is testable without a shell.
pub fn legs_of(args: &serde_json::Value, cfg: &OperatorConfig) -> Result<Vec<Leg>, ToolFailure> {
    // `{legs: [...]}` is the declared shape. A bare array reaches here when the one positional
    // argument of `sh([...])` is passed through whole; both are the same call.
    let raw = match args.get("legs") {
        Some(v) => v.clone(),
        None if args.is_array() => args.clone(),
        None => serde_json::Value::Null,
    };
    let legs: Vec<Leg> = serde_json::from_value(raw).map_err(|e| {
        err(
            FailureClass::Denied,
            format!("`sh` takes `{{legs: [{{cmd, tags}}, …]}}`: {e}"),
        )
    })?;
    if legs.is_empty() {
        return Err(err(FailureClass::Denied, "`sh` needs at least one leg"));
    }
    if legs.len() > cfg.sh_max_legs {
        return Err(err(
            FailureClass::Denied,
            format!(
                "`sh` takes at most {} legs at a time; it was given {}",
                cfg.sh_max_legs,
                legs.len()
            ),
        ));
    }
    for (i, leg) in legs.iter().enumerate() {
        if leg.cmd.trim().is_empty() {
            return Err(err(
                FailureClass::Denied,
                format!("`sh` leg {i} has no `cmd`"),
            ));
        }
        let n = leg_tags(&leg.tags).len();
        if !(cfg.sh_tags_min..=cfg.sh_tags_max).contains(&n) {
            return Err(err(
                FailureClass::Denied,
                format!(
                    "`sh` leg {i} (`{}`) needs {}-{} tags naming what it is about; it carried {n}",
                    leg.cmd, cfg.sh_tags_min, cfg.sh_tags_max
                ),
            ));
        }
    }
    Ok(legs)
}

impl Sh {
    /// Run the legs concurrently and answer in leg order.
    ///
    /// No `ToolCx`: the only thing the pipeline contributes is the cancellation token, so the
    /// body is drivable from a test that has no kernel.
    pub async fn run_legs(
        &self,
        legs: &[Leg],
        cancel: &CancellationToken,
    ) -> Result<Vec<LegResult>, ToolFailure> {
        let timeout = std::time::Duration::from_millis(self.cfg.sh_timeout_ms);
        let mut set = tokio::task::JoinSet::new();
        for (i, leg) in legs.iter().enumerate() {
            let cmd = leg.cmd.clone();
            let root = self.root.clone();
            set.spawn(async move {
                let started = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    // Cancellation and timeout are real only because dropping the future drops
                    // the child: the same rule `tools-baseline`'s `bash` holds.
                    .kill_on_drop(true)
                    .spawn();
                let out = match started {
                    Ok(c) => match tokio::time::timeout(timeout, c.wait_with_output()).await {
                        Ok(Ok(o)) => {
                            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                            text.push_str(&String::from_utf8_lossy(&o.stderr));
                            LegResult {
                                code: o.status.code().unwrap_or(-1),
                                out: text,
                            }
                        }
                        Ok(Err(e)) => LegResult {
                            code: -1,
                            out: format!("[leg failed: {e}]"),
                        },
                        Err(_) => LegResult {
                            code: -1,
                            out: format!("[leg exceeded {}ms]", timeout.as_millis()),
                        },
                    },
                    Err(e) => LegResult {
                        code: -1,
                        out: format!("[could not start `sh`: {e}]"),
                    },
                };
                (i, out)
            });
        }

        let mut results: Vec<Option<LegResult>> = (0..legs.len()).map(|_| None).collect();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    set.abort_all();
                    return Err(err(FailureClass::Cancelled, "`sh` was cancelled"));
                }
                joined = set.join_next() => match joined {
                    None => break,
                    Some(Ok((i, out))) => results[i] = Some(out),
                    Some(Err(e)) => {
                        return Err(err(FailureClass::Error, format!("`sh` lost a leg: {e}")))
                    }
                },
            }
        }
        Ok(results
            .into_iter()
            .map(|r| {
                r.unwrap_or(LegResult {
                    code: -1,
                    out: "[leg produced nothing]".to_string(),
                })
            })
            .collect())
    }
}

/// The text a person (and the typed model) reads. The VALUE carries the shape a program reads.
pub fn render(legs: &[Leg], results: &[LegResult]) -> String {
    let mut content = String::new();
    for (leg, r) in legs.iter().zip(results.iter()) {
        content.push_str(&format!(
            "$ {}\n{}\n[exit status: {}]\n",
            leg.cmd, r.out, r.code
        ));
    }
    content
}

#[async_trait::async_trait]
impl Tool for Sh {
    /// The concurrency this tool exists for is INSIDE one call. Two `sh` calls overlapping is the
    /// same hazard two `bash` calls overlapping is, and the seam already refuses that.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let legs = legs_of(&call.args, &self.cfg)?;
        let results = self.run_legs(&legs, &cx.cancel).await?;
        Ok(ToolOutcome {
            content: render(&legs, &results),
            value: Some(serde_json::to_value(&results).unwrap_or(serde_json::Value::Null)),
            // No cites: a shell result is a THOUGHT unless something else vouches for it, exactly
            // as `bash`'s is (P2-D26).
            ..Default::default()
        })
    }
}
