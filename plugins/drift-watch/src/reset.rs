//! Invariant (§8): the reset touches the DIGEST and the ABOUT-LINE and nothing else. Sealed tiers
//! are counted before and after and reported; nothing here writes one. The intent half of the
//! fresh about-line starts EMPTY — a reset that carried the old intent forward would be exactly
//! the drift it is meant to undo.

use bough_plugin_about_line::{AboutLine, ABOUT_LINE};
use bough_plugin_ledger::{Append, Cite, Class, Ref, Step, StepId, StepType, WakeId};
use bough_plugin_rollups::DigestRequest;

use crate::vocabulary::{DriftReset, DRIFT_RESET};
use crate::{DriftError, DriftInner, ResetReport, ResetRequest};

/// Run the reset.
pub async fn run(inner: &DriftInner, req: &ResetRequest) -> Result<ResetReport, DriftError> {
    let ledger = &inner.ledger;

    // Counted BEFORE anything is written, and again after: §8's "sealed tiers untouched" is a
    // reported number, not a promise in a comment.
    let tiers_before = crate::count_tiers(inner, &req.traj).await?;

    let (window, steps) = crate::read_window(inner, &req.traj).await?;
    let signals = crate::signals::compute(&req.agent, window, &steps, &inner.cfg);

    let evidence = raw_evidence(&steps, inner.cfg.max_evidence_cites);
    if evidence.is_empty() {
        // A rebuild "from raw evidence" with no raw evidence would have to invent the state half,
        // and the ledger would refuse the evidence-class step anyway. Refuse first, and say why.
        return Err(DriftError::NoEvidence(req.agent.to_string()));
    }

    // The digest is rebuilt through the SEAM: `from_raw` is what makes the provider ignore the
    // standing digest and read raw steps, and the provider — not this row — owns sealing.
    let digest = inner
        .rollups
        .0
        .rebuild_digest(&DigestRequest {
            agent: req.agent.clone(),
            traj: req.traj.clone(),
            at: req.at,
            attribution: req.attribution.clone(),
            from_raw: true,
            // A `/reset` rebuilds THIS agent's own standing digest, never an inherited one.
            parents: Vec::new(),
            reconcile: false,
        })
        .await?;

    // One synthetic wake for the whole act, derived from the digest so it is stable and unique
    // without a clock or a random source (the `WakeId::seed` precedent).
    let wake = WakeId::new(format!("drift-reset:{}", digest.digest));

    let about = ledger
        .0
        .append(Append {
            traj: req.traj.clone(),
            wake: wake.clone(),
            kind: StepType::new(ABOUT_LINE),
            class: Class::Evidence,
            body: serde_json::to_value(AboutLine {
                state: state_from_raw(&steps, &evidence, inner.cfg.max_state_chars),
                // §8: the intent half starts EMPTY.
                intent: String::new(),
                of_wake: wake.clone(),
            })
            .expect("AboutLine serialises"),
            cites: evidence.iter().map(cite_step).collect(),
            at: req.at,
            id: None,
        })
        .await?;

    let mut cites: Vec<Cite> = evidence.iter().map(cite_step).collect();
    cites.push(Cite {
        r#ref: Ref::step(&about.id),
        url: None,
    });
    cites.push(Cite {
        r#ref: Ref::rollup(&digest.digest),
        url: None,
    });
    let reset_step = ledger
        .0
        .append(Append {
            traj: req.traj.clone(),
            wake,
            kind: StepType::new(DRIFT_RESET),
            class: Class::Evidence,
            body: serde_json::to_value(DriftReset {
                agent: req.agent.clone(),
                digest: digest.digest.clone(),
                about_line: about.id.clone(),
                signals: signals.clone(),
                attribution: req.attribution.clone(),
            })
            .expect("DriftReset serialises"),
            cites,
            at: req.at,
            id: None,
        })
        .await?;

    // The seam's contract says `rebuild_digest` repoints `agents.digest_rollup`. This row CHECKS
    // rather than trusts, and repoints if the provider did not: the identity band reading a
    // digest the reset replaced is the one outcome a reset must not leave behind. Idempotent — a
    // provider that already repointed makes this a no-op read.
    if let Some(mut row) = ledger.0.agent(&req.agent).await? {
        if row.digest_rollup.as_ref() != Some(&digest.digest) {
            row.digest_rollup = Some(digest.digest.clone());
            ledger.0.put_agent(row).await?;
        }
    }

    let tiers_after = crate::count_tiers(inner, &req.traj).await?;

    // The intent half is READ BACK out of the row that was actually appended, never restated as
    // the literal this function passed in: an invariant whose input is a constant cannot fail,
    // and one that cannot fail is not an invariant.
    let intent = ledger
        .0
        .step(&about.id)
        .await?
        .and_then(|s| serde_json::from_value::<AboutLine>((*s.body).clone()).ok())
        .map(|l| l.intent)
        .unwrap_or_else(|| "<unreadable>".to_string());
    crate::invariant::record(crate::invariant::Obs {
        reset_step: reset_step.id.clone(),
        about_line: about.id.clone(),
        intent,
        tiers_before,
        tiers_after,
    });

    Ok(ResetReport {
        digest: digest.digest,
        replaced_digest: digest.replaced,
        about_line: about.id,
        reset_step: reset_step.id,
        tiers_before,
        tiers_after,
    })
}

/// The raw steps a rebuild reads, newest first, capped at [`MAX_EVIDENCE_CITES`].
///
/// `max_cites` is a config field, not a constant: the state half is a line, and how many
/// citations a line may rest on is a deployment choice (§0.2).
///
/// PURE. "Raw" means the agent's own trajectory rows — never a rollup and never another reset's
/// about-line: a reset that cited the identity it is replacing would be rebuilding from itself.
pub fn raw_evidence(steps: &[Step], max_cites: usize) -> Vec<StepId> {
    let mut out: Vec<StepId> = steps
        .iter()
        .rev()
        .filter(|s| is_raw(s))
        .map(|s| s.id.clone())
        .take(max_cites)
        .collect();
    out.reverse();
    out
}

/// Whether a step is raw evidence of what the agent did, rather than a summary of it.
fn is_raw(s: &Step) -> bool {
    !matches!(
        s.kind.as_str(),
        ABOUT_LINE | DRIFT_RESET | "rollup/sealed" | "memory/expired" | "rollup/request"
    )
}

fn cite_step(id: &StepId) -> Cite {
    Cite {
        r#ref: Ref::step(id),
        url: None,
    }
}

/// PURE: the STATE half, rebuilt from raw evidence.
///
/// It says what the raw rows say and nothing more — counts, and the last thing the agent actually
/// did. There is no model call here: §8's reset is a rebuild FROM EVIDENCE, and a sentence a model
/// wrote about the evidence would be a new claim rather than a restatement of an old one.
pub fn state_from_raw(steps: &[Step], evidence: &[StepId], max_chars: usize) -> String {
    let thoughts = steps
        .iter()
        .filter(|s| s.kind.as_str() == crate::signals::THOUGHT_TEXT)
        .count();
    let calls = steps
        .iter()
        .filter(|s| s.kind.as_str() == crate::signals::TOOL_CALL)
        .count();
    let mut out = format!(
        "rebuilt from raw evidence: {} step{} read, {thoughts} thought{}, {calls} tool call{}",
        evidence.len(),
        plural(evidence.len()),
        plural(thoughts),
        plural(calls),
    );
    if let Some(last) = steps.iter().rev().find(|s| is_raw(s)) {
        out.push_str("; last: ");
        out.push_str(&one_line(last));
    }
    if out.chars().count() > max_chars {
        out = out
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "…";
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// One raw step as a single line of the state half.
fn one_line(s: &Step) -> String {
    let detail = match s.kind.as_str() {
        crate::signals::THOUGHT_TEXT => s.body.get("text").and_then(|v| v.as_str()).map(first_line),
        crate::signals::TOOL_CALL => s
            .body
            .get("name")
            .and_then(|v| v.as_str())
            .map(|n| format!("ran `{n}`")),
        _ => None,
    };
    match detail {
        Some(d) => format!("{} — {d}", s.kind),
        None => s.kind.to_string(),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 120 {
        line.chars().take(119).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    /// The values `bough-base` carries, so a unit test and the bundle cannot drift apart.
    const MAX_EVIDENCE_CITES: usize = 24;
    const MAX_STATE_CHARS: usize = 400;

    use super::*;
    use bough_plugin_ledger::{Seq, TrajId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(seq: u64, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant"),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::<Ref>::new()),
            ignorable: false,
        }
    }

    #[test]
    fn evidence_is_raw_rows_only_and_is_bounded() {
        let mut steps = vec![
            step(
                1,
                "about/line",
                serde_json::json!({ "state": "s", "intent": "i", "of_wake": "w" }),
            ),
            step(2, "rollup/sealed", serde_json::json!({})),
        ];
        assert!(
            raw_evidence(&steps, MAX_EVIDENCE_CITES).is_empty(),
            "a summary is not raw evidence"
        );

        for seq in 3..3 + (MAX_EVIDENCE_CITES as u64 * 2) {
            steps.push(step(
                seq,
                "thought/text",
                serde_json::json!({ "text": "x" }),
            ));
        }
        let ev = raw_evidence(&steps, MAX_EVIDENCE_CITES);
        assert_eq!(ev.len(), MAX_EVIDENCE_CITES);
        // The NEWEST rows survive the cap, in seq order.
        assert_eq!(ev.last().unwrap().as_str(), "s50");
        assert_eq!(ev.first().unwrap().as_str(), "s27");
    }

    #[test]
    fn the_state_half_restates_the_raw_rows() {
        let steps = vec![
            step(1, "thought/text", serde_json::json!({ "text": "thinking" })),
            step(2, "tool/call", serde_json::json!({ "name": "bash" })),
        ];
        let ev = raw_evidence(&steps, MAX_EVIDENCE_CITES);
        let state = state_from_raw(&steps, &ev, MAX_STATE_CHARS);
        assert!(state.contains("2 steps read"), "{state}");
        assert!(state.contains("1 thought,"), "{state}");
        assert!(state.contains("1 tool call"), "{state}");
        assert!(state.contains("ran `bash`"), "{state}");
        assert!(state.chars().count() <= MAX_STATE_CHARS);
    }
}
