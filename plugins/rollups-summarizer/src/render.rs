//! Invariant: rendering is PURE, which is what makes the offline suite deterministic. And the
//! index never depends on the model's discipline: a model that returns prose and no structure
//! still yields a block whose `evidence` comes from the WINDOW, not from the answer (P4-D17).

use std::collections::BTreeSet;

use bough_plugin_ledger::{Class, Ref, Rollup, Step, StepId};
use bough_plugin_rollups::{Beneath, Inputs, RollupsError, Theme, TierBlock, Window, WindowRef};

use crate::call::Phase;
use crate::SummarizerConfig;

/// How much of one step's body a rendered line may carry. A protocol constant, not a tunable:
/// it is the shape of the rendered line, and a deployment does not vary it.
const STEP_LINE_CHARS: usize = 240;

/// The recap prompt, versioned.
///
/// `None` when the binary has no prompt for `(phase, ver)`; [`crate::resolve::validate`] turns
/// that into a boot refusal.
pub fn system_prompt(phase: Phase, ver: &str) -> Option<&'static str> {
    crate::prompts::lookup(phase, ver)
}

/// Truncate to `max` CHARACTERS, never bytes: a block is text and a split codepoint is a bug.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

/// One step body flattened to a single line. Deterministic: `serde_json` orders object keys.
fn one_line(step: &Step) -> String {
    let body = step.body.as_ref();
    let head = ["text", "title", "summary", "subject", "name", "reason"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| serde_json::to_string(body).unwrap_or_default());
    let flat = head.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&flat, STEP_LINE_CHARS)
}

/// One episode window as the model sees it: `[seq] kind: one line`, thoughts marked as thoughts,
/// evidence carrying its cites.
pub fn render_window(steps: &[Step], w: &Window) -> String {
    let want: BTreeSet<&StepId> = w.steps.iter().collect();
    let mut out = format!(
        "episode {}..{} ({} steps, cut: {})\n",
        w.from_seq.0,
        w.to_seq.0,
        w.steps.len(),
        serde_json::to_value(w.cut)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "head".into())
    );
    for s in steps.iter().filter(|s| want.contains(&s.id)) {
        let mark = match s.class {
            Class::Thought => "thought",
            Class::Evidence => "evidence",
        };
        out.push_str(&format!(
            "[{}] {} · {mark}: {}\n",
            s.seq.0,
            s.kind,
            one_line(s)
        ));
        if s.class == Class::Evidence && !s.cites.is_empty() {
            let cites = s
                .cites
                .iter()
                .map(|c| c.r#ref.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("      cites: {cites}\n"));
        }
    }
    out
}

/// The `text` a sealed block carries, read back out of its body. Total: a body this crate did not
/// write still yields a string rather than a panic.
pub fn block_text(r: &Rollup) -> String {
    r.body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// `fanout` child blocks as the reduce sees them.
pub fn render_children(children: &[Rollup]) -> String {
    let mut out = String::new();
    for c in children {
        out.push_str(&format!(
            "block {} (tier {}, steps {}..{})\n{}\n\n",
            c.id,
            c.tier,
            c.from_seq.0,
            c.to_seq.0,
            block_text(c)
        ));
    }
    out
}

/// The evidence a block carries: RAW step ids drawn from what the block covers, capped.
fn evidence_of(steps: &[Step], cfg: &SummarizerConfig) -> Vec<StepId> {
    steps
        .iter()
        .map(|s| s.id.clone())
        .take(cfg.max_evidence_refs)
        .collect()
}

/// Split the answer into its prose head and its `## title` theme sections.
fn split_themes(answer: &str) -> (String, Vec<(String, String)>) {
    let mut head = String::new();
    let mut themes: Vec<(String, String)> = Vec::new();
    for line in answer.lines() {
        match line.strip_prefix("## ") {
            Some(title) => themes.push((title.trim().to_string(), String::new())),
            None => match themes.last_mut() {
                Some((_, body)) => {
                    body.push_str(line.trim());
                    body.push('\n');
                }
                None => {
                    head.push_str(line);
                    head.push('\n');
                }
            },
        }
    }
    (head.trim().to_string(), themes)
}

/// Parse the model's answer into a block.
///
/// A model that returns prose and no structure still yields a block: `text` is the prose,
/// `themes` is empty, and `evidence` comes from the window, never from the model.
pub fn parse_block(
    answer: &str,
    inputs: &Inputs,
    steps: &[Step],
    cfg: &SummarizerConfig,
) -> Result<TierBlock, RollupsError> {
    let (head, sections) = split_themes(answer);
    // A block with no prose at all is not a recap; the pass says so rather than sealing a hole.
    if head.trim().is_empty() && sections.is_empty() {
        return Err(RollupsError::BadBlock(
            "the answer carried no recap prose at all".to_string(),
        ));
    }
    let evidence = evidence_of(steps, cfg);
    // The refs a theme is "about" are the covered steps' own refs, capped the same way the
    // `notable_refs` column is: a ref the model typed is not a ref the ledger knows.
    let refs: Vec<Ref> = steps
        .iter()
        .flat_map(|s| s.refs.iter().cloned())
        .collect::<BTreeSet<Ref>>()
        .into_iter()
        .take(cfg.max_notable_refs)
        .collect();
    let text = if head.trim().is_empty() {
        // Prose-free but themed: the first theme's text stands in, so `text` is never empty.
        sections
            .first()
            .map(|(t, b)| format!("{t}: {}", b.trim()))
            .unwrap_or_default()
    } else {
        head
    };
    Ok(TierBlock {
        text: truncate(&text, cfg.max_block_chars),
        themes: sections
            .into_iter()
            .map(|(title, body)| Theme {
                title,
                text: truncate(body.trim(), cfg.max_block_chars),
                refs: refs.clone(),
                evidence: evidence.clone(),
            })
            .collect(),
        beneath: match inputs {
            Inputs::Raw(ids) => Beneath::Raw { steps: ids.clone() },
            Inputs::Blocks(ids) => Beneath::Blocks {
                rollups: ids.clone(),
            },
        },
        evidence,
        windows: Vec::new(),
        tier: 0,
        prompt_ver: cfg.prompt_ver.clone(),
    })
}

/// Stamp the block with the tier and the windows it covers. Separate from [`parse_block`] because
/// the tier and the window shape are the PLANNER's facts, never the model's.
pub fn stamp(block: &mut TierBlock, tier: u8, windows: &[Window]) {
    block.tier = tier;
    block.windows = windows
        .iter()
        .map(|w| WindowRef {
            from_seq: w.from_seq,
            to_seq: w.to_seq,
            cut: w.cut,
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Seq, StepType, TrajId, WakeId};
    use bough_plugin_rollups::Cut;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn cfg() -> SummarizerConfig {
        SummarizerConfig {
            prompt_ver: crate::prompts::R4_1.to_string(),
            gap_minutes: 45,
            max_window_steps: 10,
            min_window_steps: 2,
            fanout: 10,
            max_tier: 3,
            seal_lag_steps: 20,
            max_calls_per_pass: 8,
            max_notable_refs: 12,
            max_evidence_refs: 24,
            max_block_chars: 1200,
            map_max_tokens: 1024,
            reduce_max_tokens: 1536,
        }
    }

    fn step(seq: u64, kind: &str, class: Class, text: &str) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("t"),
            seq: Seq(seq),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            wake: WakeId::new("w"),
            kind: StepType::new(kind),
            class,
            body: Arc::new(serde_json::json!({ "text": text })),
            cites: Arc::new(if class == Class::Evidence {
                vec![Cite {
                    r#ref: Ref::new("gh:o/r#1"),
                    url: None,
                }]
            } else {
                vec![]
            }),
            refs: Arc::new(
                if class == Class::Evidence {
                    vec![Ref::new("gh:o/r#1")]
                } else {
                    vec![]
                }
                .into_iter()
                .collect(),
            ),
            ignorable: false,
        }
    }

    fn steps() -> Vec<Step> {
        vec![
            step(1, "thought/text", Class::Thought, "read the plan"),
            step(2, "tool/result", Class::Evidence, "the tests are green"),
        ]
    }

    fn window(steps: &[Step]) -> Window {
        Window {
            from_seq: steps[0].seq,
            to_seq: steps[steps.len() - 1].seq,
            from_at: steps[0].at,
            to_at: steps[steps.len() - 1].at,
            steps: steps.iter().map(|s| s.id.clone()).collect(),
            cut: Cut::Gap,
        }
    }

    /// The reason the offline suite can have a golden at all: same steps, same string, every run.
    #[test]
    fn a_window_renders_deterministically() {
        let s = steps();
        let w = window(&s);
        let once = render_window(&s, &w);
        assert_eq!(once, render_window(&s, &w), "rendering is pure");
        assert!(once.contains("episode 1..2"), "{once}");
        assert!(
            once.contains("[1] thought/text · thought: read the plan"),
            "{once}"
        );
        assert!(once.contains("[2] tool/result · evidence:"), "{once}");
        assert!(once.contains("cites: gh:o/r#1"), "{once}");
        // A step outside the window is not rendered, whatever else the caller passed in.
        let mut extra = s.clone();
        extra.push(step(9, "thought/text", Class::Thought, "later"));
        assert_eq!(render_window(&extra, &w), once);
    }

    /// P4-D17: the prose may be the model's; the index may not be.
    #[test]
    fn a_prose_only_answer_still_yields_a_block_whose_evidence_comes_from_the_window() {
        let s = steps();
        let inputs = Inputs::Raw(s.iter().map(|x| x.id.clone()).collect());
        let block = parse_block(
            "We read the plan and the tests came back green. step:invented-by-the-model",
            &inputs,
            &s,
            &cfg(),
        )
        .expect("prose alone is still a block");
        assert!(block.themes.is_empty(), "no structure was offered");
        assert_eq!(
            block.evidence,
            vec![StepId::new("s1"), StepId::new("s2")],
            "evidence is the window's, not the answer's"
        );
        assert!(matches!(&block.beneath, Beneath::Raw { steps } if steps.len() == 2));
        assert_eq!(block.prompt_ver, cfg().prompt_ver);
        // A themed answer keeps both halves, and the themes carry the same window evidence.
        let themed = parse_block(
            "Head prose.\n## Tests\nThey went green.\n## Plan\nStill open.",
            &inputs,
            &s,
            &cfg(),
        )
        .expect("a themed answer parses");
        assert_eq!(themed.text, "Head prose.");
        assert_eq!(themed.themes.len(), 2);
        assert_eq!(themed.themes[0].title, "Tests");
        assert_eq!(themed.themes[0].evidence, themed.evidence);
        // An empty answer is a refusal, not an empty block.
        assert!(matches!(
            parse_block("   \n ", &inputs, &s, &cfg()),
            Err(RollupsError::BadBlock(_))
        ));
    }

    #[test]
    fn a_block_is_truncated_to_max_block_chars() {
        let s = steps();
        let inputs = Inputs::Raw(s.iter().map(|x| x.id.clone()).collect());
        let mut c = cfg();
        c.max_block_chars = 20;
        let long = "é".repeat(500);
        let block = parse_block(&format!("{long}\n## T\n{long}"), &inputs, &s, &c)
            .expect("a long answer still parses");
        assert_eq!(block.text.chars().count(), 20, "text: {}", block.text);
        assert_eq!(block.themes[0].text.chars().count(), 20);
        assert!(block.text.ends_with('…'), "the cut is visible");
        // And a short one is left exactly alone.
        assert_eq!(truncate("short", 20), "short");
    }
}
