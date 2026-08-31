//! Drivability (2026-08-31): the dialogue band keeps the THREAD when a tool-heavy wake would
//! evict it from the step-counted tail — the "couldn't follow along" conversation, as a test.
//! Both providers, because the band is a ledger query like any other.

use crate::support::{self, Harness, Which};
use bough_plugin_ledger::{Cite, Class, Ref};
use bough_plugin_projection_assembler::AssemblerConfig;

async fn conversation_then_tool_spam(h: &Harness) {
    // The conversation vocabulary (`mail/delivered`, `thought/text`) is `agents`-owned and
    // schema-checked; register it the way the loop's own boot does.
    for def in bough_plugin_agents::vocabulary::step_types() {
        drop(h.ledger.0.register_step_type(def));
    }
    h.put_agent(None).await;
    // The conversation: what Andrey said, and what the agent answered.
    h.append(
        "m1",
        "w1",
        "mail/delivered",
        Class::Evidence,
        serde_json::json!({
            "class": "wake", "from": "andrey", "refs": [],
            "subject": "look at the deepseek harness setup",
            "summary": "look at the deepseek harness setup",
        }),
        // Evidence requires a cite; the loop cites the splice that queued the message.
        vec![Cite {
            r#ref: Ref::new("step:s0"),
            url: None,
        }],
    )
    .await;
    h.append(
        "t1",
        "w1",
        "thought/text",
        Class::Thought,
        serde_json::json!({ "text": "dsh findings so far", "step_index": 0 }),
        Vec::new(),
    )
    .await;
    // A tool-heavy wake: more noise steps than the tail holds (`support::cfg` selects 12).
    // `note` appends the harness's own non-conversation step kind — what matters is only that
    // these are NOT `mail/delivered`/`thought/text` and that there are more of them than the
    // tail window.
    for i in 0..20 {
        h.note(&format!("p{i:02}"), "w2", &format!("noise {i}")).await;
    }
}

#[tokio::test]
async fn the_dialogue_band_keeps_the_thread_a_tool_heavy_wake_evicts() {
    for which in [Which::Memory, Which::Sqlite] {
        let h = Harness::open(which);
        conversation_then_tool_spam(&h).await;
        let out = h
            .assemble(AssemblerConfig {
                dialogue_steps: 4,
                ..support::cfg(100_000)
            })
            .await;
        // The tail is nothing but the spam wake…
        let tail = out
            .sections
            .iter()
            .find(|s| s.id.as_str() == "tail")
            .expect("a tail");
        assert!(
            !tail.body.contains("deepseek harness"),
            "the premise: the conversation fell out of the tail — {}",
            tail.body
        );
        // …and the dialogue band carries the words anyway, cited.
        let dlg = out
            .sections
            .iter()
            .find(|s| s.id.as_str() == "dialogue")
            .expect("the dialogue band");
        assert!(dlg.body.contains("look at the deepseek harness setup"), "{}", dlg.body);
        assert!(dlg.body.contains("dsh findings so far"), "{}", dlg.body);
        assert!(
            !dlg.body.contains("noise"),
            "only conversation kinds, never tool output: {}",
            dlg.body
        );
        assert_eq!(
            dlg.cites.steps.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["m1", "t1"],
            "model-visible ⟺ ledgered"
        );
        // Ordered right before the tail, wherever the two land.
        let ids = support::ids(&out);
        let d = ids.iter().position(|i| i == "dialogue").unwrap();
        let t = ids.iter().position(|i| i == "tail").unwrap();
        assert!(d < t, "{ids:?}");

        // `0` = off: exactly yesterday's projection, so every golden holds.
        let out = h.assemble(support::cfg(100_000)).await;
        assert!(
            !support::ids(&out).iter().any(|i| i == "dialogue"),
            "dialogue_steps: 0 renders nothing"
        );
    }
}
