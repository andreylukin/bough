//! Invariant: a ledger drill is EVIDENCE. Every result cites the steps it came from, so reading
//! the past is as citable as observing the present.

use std::sync::Arc;

use bough_plugin_ledger::{
    AgentName, Cite, LedgerHandle, Order, Ref, SearchQuery, Seq, Step, StepQuery,
};
use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

use crate::OperatorConfig;

/// One tool — `{op: "search"|"steps"|"tail", ...}` — sugared as the `ledger` namespace:
/// `ledger.search(q)` / `ledger.steps(range)` / `ledger.tail(n)`. The point is drilling from a
/// tier's `notable_refs` down to the raw steps behind them.
pub struct LedgerRead {
    pub cfg: Arc<OperatorConfig>,
    pub ledger: LedgerHandle,
}

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

/// The cite that makes a drill evidence.
pub fn step_cite(step: &Step) -> Cite {
    Cite {
        r#ref: Ref::new(format!("step:{}", step.id)),
        url: None,
    }
}

/// One rendered row. Deliberately short: the model drills again for a body it wants in full.
pub fn render(step: &Step, snippet: Option<&str>) -> String {
    let body = match snippet {
        Some(s) => s.to_string(),
        None => step.body.to_string(),
    };
    let body = body.replace('\n', " ");
    let body: String = body.chars().take(240).collect();
    format!(
        "#{} {} {} [{}] {}",
        step.seq.0, step.traj, step.kind, step.id, body
    )
}

/// Everything the three ops share: the page bound and the "cited, therefore evidence" rule.
fn page(cfg: &OperatorConfig, asked: Option<usize>) -> usize {
    asked.unwrap_or(cfg.ledger_page).clamp(1, cfg.ledger_page)
}

fn outcome(header: String, steps: Vec<(Step, Option<String>)>) -> ToolOutcome {
    let mut lines = vec![header];
    let mut cites = Vec::new();
    for (s, snip) in &steps {
        lines.push(render(s, snip.as_deref()));
        cites.push(step_cite(s));
    }
    if steps.is_empty() {
        lines.push("(nothing)".to_string());
    }
    ToolOutcome {
        content: lines.join("\n"),
        value: Some(serde_json::json!({
            "count": steps.len(),
            "steps": steps
                .iter()
                .map(|(s, _)| serde_json::json!({
                    "id": s.id.as_str(),
                    "traj": s.traj.as_str(),
                    "seq": s.seq.0,
                    "kind": s.kind.as_str(),
                }))
                .collect::<Vec<_>>(),
        })),
        cites,
        concludes_wake: false,
    }
}

/// The whole tool as one function of `(ledger, cfg, agent, args)`, so a test can drive a drill
/// without standing up a pipeline — and so the pipeline path and the test path are the same code.
pub async fn drill(
    ledger: &LedgerHandle,
    cfg: &OperatorConfig,
    agent: &AgentName,
    args: &serde_json::Value,
) -> Result<ToolOutcome, ToolFailure> {
    let op = args
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| err(FailureClass::Error, "`op` is required and must be a string"))?;
    // Membership is derived AT NEED (§3): a drill reads exactly what the agent is connected to,
    // never the whole store.
    let connected = ledger
        .0
        .connected(agent)
        .await
        .map_err(|e| err(FailureClass::Error, e.to_string()))?;
    let trajs: Vec<_> = connected.trajectories().into_iter().collect();
    match op {
        "search" => {
            let text = args
                .get("q")
                .and_then(|v| v.as_str())
                .ok_or_else(|| err(FailureClass::Error, "`q` is required for a search"))?;
            let limit = page(
                cfg,
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize),
            );
            let hits = ledger
                .0
                .search(&SearchQuery {
                    text: text.to_string(),
                    trajs,
                    limit,
                })
                .await
                .map_err(|e| err(FailureClass::Error, e.to_string()))?;
            let n = hits.len();
            Ok(outcome(
                format!("ledger.search({text:?}) — {n} hit(s), page {limit}"),
                hits.into_iter()
                    .map(|h| (h.step, Some(h.snippet)))
                    .collect(),
            ))
        }
        "steps" => {
            let after = args.get("from").and_then(|v| v.as_u64()).map(Seq);
            let before = args.get("to").and_then(|v| v.as_u64()).map(Seq);
            let limit = page(
                cfg,
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize),
            );
            let steps = ledger
                .0
                .steps(&StepQuery {
                    trajs,
                    after,
                    before,
                    order: Order::SeqAsc,
                    limit: Some(limit),
                    ..Default::default()
                })
                .await
                .map_err(|e| err(FailureClass::Error, e.to_string()))?;
            let n = steps.len();
            Ok(outcome(
                format!(
                    "ledger.steps({}..{}) — {n} step(s), page {limit}",
                    after.map(|s| s.0.to_string()).unwrap_or_else(|| "".into()),
                    before.map(|s| s.0.to_string()).unwrap_or_else(|| "".into()),
                ),
                steps.into_iter().map(|s| (s, None)).collect(),
            ))
        }
        "tail" => {
            let n = page(
                cfg,
                args.get("n").and_then(|v| v.as_u64()).map(|n| n as usize),
            );
            if connected.is_rowless() {
                return Ok(outcome(format!("ledger.tail({n}) — no trajectory"), vec![]));
            }
            let steps = ledger
                .0
                .tail(&connected.own, n)
                .await
                .map_err(|e| err(FailureClass::Error, e.to_string()))?;
            let got = steps.len();
            Ok(outcome(
                format!("ledger.tail({n}) — {got} step(s)"),
                steps.into_iter().map(|s| (s, None)).collect(),
            ))
        }
        other => Err(err(
            FailureClass::Error,
            format!("`op` must be one of search|steps|tail, not `{other}`"),
        )),
    }
}

#[async_trait::async_trait]
impl Tool for LedgerRead {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        drill(&self.ledger, &self.cfg, &call.agent, &call.args).await
    }
}
