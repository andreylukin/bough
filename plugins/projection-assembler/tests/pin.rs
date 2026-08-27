//! §10, P5-D12 — the pinned prefix. What this file pins:
//!
//! * a pinned agent's `assemble` returns the pinned bytes VERBATIM, at any budget and any
//!   `as_of`: it is a replay, not an assembly;
//! * an agent with no pin assembles normally, and a pin on one agent is invisible to another.

mod support;

use std::sync::Arc;

use bough_plugin_ledger::{AgentName, Seq};
use bough_plugin_projection::{AssembleRequest, PrefixSource, Projector};
use bough_plugin_projection_assembler::Assembler;
use support::*;

/// An assembler over the harness's ledger. Built here rather than on `Harness`: the pin store
/// lives on the assembler, so a case that pins and then assembles must hold the SAME one.
fn assembler(h: &Harness) -> Arc<Assembler> {
    Assembler::new(Arc::new(cfg(100_000)), h.ledger.clone(), h.ctx.clone())
}

fn request(agent: &str, budget: Option<usize>) -> AssembleRequest {
    AssembleRequest {
        agent: AgentName::new(agent),
        wake: None,
        at: at(),
        budget,
        as_of: None,
    }
}

#[tokio::test]
async fn a_pinned_prefix_is_byte_identical_to_what_was_pinned() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    h.pin("p1", "a standing rule", "gates green before every commit")
        .await;
    for n in 1..=8 {
        h.note(&format!("s{n}"), "w1", "a step of some length")
            .await;
    }
    let assembler = assembler(&h);
    let parent = assembler
        .assemble(&request("sol", None))
        .await
        .expect("the parent assembles");

    // The child is a different agent with NO rows of its own.
    let child = AgentName::new("sol/worker-fork-1");
    let token = assembler
        .pin_prefix(
            child.clone(),
            parent.clone(),
            PrefixSource {
                of_agent: agent(),
                as_of: Seq(1),
            },
        )
        .expect("pinning does not fail");

    // Any budget, any `as_of`: the same bytes.
    for budget in [Some(1usize), Some(100_000), None] {
        let got = assembler
            .assemble(&AssembleRequest {
                as_of: Some(Seq(1)),
                ..request(child.as_str(), budget)
            })
            .await
            .expect("a pinned agent assembles");
        assert_eq!(
            got.to_text(),
            parent.to_text(),
            "the pin was re-assembled rather than replayed at budget {budget:?}"
        );
        assert_eq!(got, parent, "and verbatim, flags and cites included");
    }

    token.remove();
    let after = assembler
        .assemble(&request(child.as_str(), None))
        .await
        .expect("an unpinned agent still assembles");
    assert_ne!(
        after.to_text(),
        parent.to_text(),
        "removing the pin must restore ordinary assembly"
    );
}

#[tokio::test]
async fn an_unpinned_agent_assembles_normally() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    h.pin("p1", "a standing rule", "gates green before every commit")
        .await;
    let assembler = assembler(&h);

    let before = assembler
        .assemble(&request("sol", None))
        .await
        .expect("assembles");
    // A pin held by SOMEONE ELSE changes nothing here.
    let _token = assembler
        .pin_prefix(
            AgentName::new("terra"),
            before.clone(),
            PrefixSource {
                of_agent: agent(),
                as_of: Seq(1),
            },
        )
        .expect("pinning does not fail");
    let after = assembler
        .assemble(&request("sol", None))
        .await
        .expect("assembles");
    assert_eq!(before, after, "another agent's pin leaked into this one");
    assert!(
        after
            .sections
            .iter()
            .any(|s| s.body.contains("gates green before every commit")),
        "the unpinned agent assembled its own pins band"
    );
}
