//! Invariant (§5, P2-D20): `request/header` is appended ONLY when it differs from the last one in
//! this wake, and it carries the composition fingerprint, the projection digest, `as_of` and the
//! budget — the four things that turn V4 into a reconstruction rather than a hash comparison.
//!
//! The COMPARISON is over §5's four ("prompt version, section ids, tool schemas, call config").
//! The three reconstruction anchors are written on every header that IS appended but take no part
//! in the comparison: `as_of` moves with every append, so comparing it would put a header on
//! every step and say nothing.

use std::sync::Arc;

use bough_plugin_ledger::vocabulary::RequestHeader;
use bough_plugin_ledger::Seq;
use bough_plugin_llm::{CallConfig, LlmMessage, LlmRequest, LlmToolDef, RequestFacts};
use bough_plugin_projection::Assembled;

/// Everything one request is built from, gathered before any of it is written down.
#[derive(Clone)]
pub struct RequestInputs {
    pub facts: Arc<RequestFacts>,
    pub projection: Assembled,
    pub as_of: Seq,
    pub budget: usize,
    pub tools: Vec<LlmToolDef>,
    pub call: CallConfig,
}

/// Build the request. Pure over its inputs — no clock, no ledger — so the invariant's
/// reconstruction runs the same function on the same inputs.
pub fn build(inputs: &RequestInputs, messages: Vec<LlmMessage>) -> LlmRequest {
    let system = inputs.projection.to_text();
    LlmRequest {
        model: inputs.call.model.clone(),
        // The projection is the STABLE prefix: bough-llm's cache contract, and §5's "context =
        // projection". Nothing volatile is sent, because everything model-visible is ledgered.
        system: (!system.is_empty()).then_some(system),
        system_volatile: None,
        messages,
        tools: inputs.tools.clone(),
        call: inputs.call.clone(),
    }
}

/// The `request/header` a set of inputs describes: §5's four, plus P2-D20's three anchors.
pub fn header_of(inputs: &RequestInputs) -> RequestHeader {
    RequestHeader {
        prompt_ver: inputs.facts.prompt_ver.clone(),
        as_of: inputs.as_of,
        budget: inputs.budget,
        projection_digest: digest(&inputs.projection.to_text()),
        sections: inputs
            .projection
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect(),
        step_index: inputs.facts.step_index,
        tools: inputs.tools.iter().map(|t| t.name.clone()).collect(),
        tools_digest: tools_digest(&inputs.tools),
        call: call_json(&inputs.call),
        composition: inputs.facts.composition.clone(),
    }
}

/// The `request/header` body for a request, or `None` when it repeats the last one in this wake.
pub fn header_if_changed(
    last: Option<&RequestHeader>,
    inputs: &RequestInputs,
) -> Option<RequestHeader> {
    let next = header_of(inputs);
    match last {
        Some(prev) if same_four(prev, &next) => None,
        _ => Some(next),
    }
}

/// §5's four — prompt version, section ids, tool SCHEMAS, call config — plus the composition
/// fingerprint and the projection digest.
///
/// `as_of` and `budget` are deliberately NOT part of the comparison: `as_of` moves with every
/// append, so comparing it would put a header on every step and say nothing. The projection
/// DIGEST is a different matter and is compared: it is the anchor V4 reconstructs the system
/// prefix from, and a step whose prefix changed with no header for it is a step whose prefix
/// nothing in the ledger describes. "Tool schemas" is likewise the digest and not the name list,
/// because a scoped tool may shadow its same-named global twin with a different schema.
pub fn same_four(a: &RequestHeader, b: &RequestHeader) -> bool {
    a.prompt_ver == b.prompt_ver
        && a.sections == b.sections
        && a.tools == b.tools
        && a.tools_digest == b.tools_digest
        && a.call == b.call
        && a.composition == b.composition
        && a.projection_digest == b.projection_digest
}

/// sha256 of the canonical JSON of the tool definitions offered, hex.
pub fn tools_digest(tools: &[LlmToolDef]) -> String {
    let canonical: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "schema": t.input_schema,
            })
        })
        .collect();
    digest(&serde_json::to_string(&canonical).unwrap_or_default())
}

/// The body actually appended.
pub fn header_body(header: &RequestHeader, _inputs: &RequestInputs) -> serde_json::Value {
    serde_json::to_value(header).expect("a header serialises")
}

/// sha256, hex. The one spelling in this crate.
pub fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// The call config, as the header stores it. Stable field order: it is a comparison key.
pub fn call_json(call: &CallConfig) -> serde_json::Value {
    serde_json::json!({
        "model": call.model,
        "max_tokens": call.max_tokens,
        "effort": call.effort,
        "tool_choice_none": call.tool_choice_none,
        "meta": call.meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{AgentName, TrajId, WakeId};
    use bough_plugin_llm::WakeKind;
    use bough_plugin_projection::{SectionCites, SectionId};

    fn assembled(body: &str) -> Assembled {
        Assembled {
            agent: AgentName::new("sol"),
            sections: vec![bough_plugin_projection::RenderedSection {
                id: SectionId::new("identity"),
                position: bough_plugin_projection::Position::band(
                    bough_plugin_projection::Slot::Identity,
                ),
                title: "identity".into(),
                body: body.into(),
                cites: SectionCites::default(),
                tokens: 1,
                degraded: None,
            }],
            flags: Default::default(),
            tokens: 1,
            budget: 100,
            cites: SectionCites::default(),
        }
    }

    fn inputs(model: &str, body: &str) -> RequestInputs {
        RequestInputs {
            facts: Arc::new(RequestFacts {
                agent: AgentName::new("sol"),
                traj: TrajId::new("lane/sol"),
                wake: WakeId::new("w1"),
                wake_kind: WakeKind::Answer,
                step_index: 0,
                answers_andrey: true,
                model_override: None,
                prompt_ver: "p2.1".into(),
                composition: "comp".into(),
            }),
            projection: assembled(body),
            as_of: Seq(7),
            budget: 100,
            tools: vec![],
            call: CallConfig {
                model: model.into(),
                max_tokens: 8192,
                effort: None,
                tool_choice_none: false,
                meta: Default::default(),
            },
        }
    }

    #[test]
    fn the_projection_is_the_systems_stable_prefix() {
        let req = build(&inputs("haiku", "hello"), vec![]);
        assert!(req.system.as_deref().unwrap().contains("hello"));
        assert_eq!(req.system_volatile, None);
        assert_eq!(req.model, "haiku");
    }

    #[test]
    fn a_header_is_appended_only_when_one_of_the_four_changes() {
        let a = inputs("haiku", "hello");
        let first = header_if_changed(None, &a).expect("the first header always lands");
        // Same four, a LATER as_of and a different projection body: still no header, because
        // §5's comparison is over the four and the tail growing is not one of them.
        let mut b = inputs("haiku", "hello");
        b.as_of = Seq(9);
        assert_eq!(header_if_changed(Some(&first), &b), None);
        // The call config changing IS one of the four.
        let c = inputs("opus", "hello");
        assert!(header_if_changed(Some(&first), &c).is_some());
    }

    #[test]
    fn the_appended_body_carries_the_three_reconstruction_anchors() {
        let i = inputs("haiku", "hello");
        let body = header_body(&header_of(&i), &i);
        assert_eq!(body["as_of"], 7);
        assert_eq!(body["budget"], 100);
        assert_eq!(body["projection_digest"], digest(&i.projection.to_text()));
        assert_eq!(body["composition"], "comp");
    }
}
