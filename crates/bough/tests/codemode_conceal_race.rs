//! The concealment must be in force for an agent's FIRST request, not shortly after it.
//!
//! `agent/created` is an EMIT event — dispatched fire-and-forget — so installing the code-mode
//! restriction from a listener on it left a window in which an agent created and woken in the
//! same breath built its first request with the WHOLE typed tool list beside `run`, while being
//! handed a surface section that says "There are no other tools and no per-call schemas — this
//! section is the whole surface." A failed install was only `tracing::error!`-logged, so such an
//! agent ran unconcealed for its whole life with nothing on screen.
//!
//! The fix is the `agent/wake-request` waterfall, which every loop Provider AWAITS immediately
//! before the wake exists. This case pins it by doing what the older case avoided: no
//! `kernel.quiesce()` between `agents.create` and the message that wakes it.

mod support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, TrajId};

fn uuid_v7() -> String {
    format!(
        "race-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// A transcript that answers in text, calling nothing: what is under test is the tool LIST the
/// request carried, not what the model did with it.
fn transcript(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("codemode-race.yml");
    let doc = serde_json::json!({
        "entries": { "llm.anthropic": {
            "plugin": "llm-replay",
            "config": { "strict": true, "models": "*", "rounds": [
                { "chunks": [
                    { "type": "text", "text": "nothing to do." },
                    { "type": "usage", "input_tokens": 900, "output_tokens": 20 },
                    { "type": "end", "stop": "end_turn" } ] },
            ]}
        }}
    });
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_request_of_a_freshly_created_agent_is_already_concealed() {
    let _guard = trace::test_lock();
    let scratch = std::env::temp_dir().join(format!("bough-codemode-race-{}", uuid_v7()));
    std::fs::create_dir_all(&scratch).unwrap();
    let patch = transcript(&scratch);
    let (kernel, _dir) = support::boot_real("codemode", &[patch]).await;

    let agents = kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: TrajId::new("lane/codemode-race"),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");

    // NO quiesce here. On the emit-only path this is the window the bug lived in.
    agent
        .followup(bough_plugin_agents::Message {
            id: bough_plugin_agents::MessageId::new(uuid_v7()),
            from: bough_plugin_agents::Sender::Andrey,
            subject: "hello".into(),
            text: "hello".into(),
            class: bough_plugin_agents::MailClass::Wake,
            refs: Default::default(),
            cites: Vec::new(),
            at: chrono::Utc::now(),
            mail_seq: None,
        })
        .await
        .expect("the message lands");
    agent.when_idle().await;

    let sent = bough_plugin_agent_loop::invariant::seen();
    assert!(!sent.is_empty(), "the loop must have sent a request");
    for req in &sent {
        let names: Vec<&str> = req.request.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["run"],
            "every request under code mode shows `run` and nothing else"
        );
    }

    disposer.dispose().await;
    kernel.shutdown().await;
    let _ = std::fs::remove_dir_all(&scratch);
}
