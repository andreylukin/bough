//! V2 — **model-visible ⟺ ledgered**, under the SECOND consumer.
//!
//! The claim code mode has to keep is the one the whole system rests on: everything the model was
//! shown can be rebuilt from the ledger, and nothing that was not shown to it leaks into the
//! rebuild. Under typed tools that is one `tool/call` per model call. Under code mode a single
//! `run` call fans out into `program/call` / `program/result` / `program/console` sub-steps that
//! the model NEVER SAW individually — it saw the console text, once, as the `run` result. So the
//! invariant is a stronger statement here than under typed tools, and it is the reason plan D-1
//! gave the sub-steps their own kinds instead of reusing `tool/call`.
//!
//! These boot in process, because what is asserted is the kernel's INVARIANT RUNNER and
//! `agent-loop`'s recorded request stream — neither survives a process boundary.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::Kernel;
use bough_plugin_agents::{AgentKind, Agents, CreateAgent};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, Ledger, Step, StepQuery, TrajId};
use support::TempDir;

/// A unique message id without pulling `uuid` into this package's dev-dependencies.
fn uuid_v7() -> String {
    format!(
        "bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// The requests the loop sent FOR THIS WAKE. The recorder is process-global, so a previous test in
/// this binary would otherwise be read as this one's evidence; the wakes in `steps` are the
/// selector because they are the same rows the reconstruction is checked against.
fn sent_for(steps: &[Step]) -> Vec<bough_plugin_agent_loop::invariant::SentRequest> {
    let wakes: std::collections::BTreeSet<_> = steps.iter().map(|s| s.wake.clone()).collect();
    bough_plugin_agent_loop::invariant::seen()
        .into_iter()
        .filter(|s| wakes.contains(&s.wake))
        .collect()
}

/// The trajectory every case in this file runs on.
fn traj() -> TrajId {
    TrajId::new("lane/codemode")
}

/// A recorded transcript as a `--patch` layer: one `run` round, then a text answer. The program
/// makes TWO host calls, so the ledger gets sub-steps the model never saw as separate messages.
///
/// `view` and not `bash`: with `tags_required` on, no registered tool has a `tags` property, so
/// every shell call in the sandbox is refused today (`docs/codemode-merge-notes.md` §9). What is
/// under test is the step relation, which any two host calls exercise.
fn transcript(dir: &std::path::Path, program: &str) -> PathBuf {
    let path = dir.join("codemode-transcript.yml");
    let doc = serde_json::json!({
        "entries": { "llm.anthropic": {
            "plugin": "llm-replay",
            "config": { "strict": true, "models": "*", "rounds": [
                { "chunks": [
                    { "type": "tool_call", "id": "c0", "name": "run",
                      "input": { "program": program } },
                    { "type": "usage", "input_tokens": 900, "output_tokens": 90 },
                    { "type": "end", "stop": "tool_use" } ] },
                { "chunks": [
                    { "type": "text", "text": "read it." },
                    { "type": "usage", "input_tokens": 900, "output_tokens": 20 },
                    { "type": "end", "stop": "end_turn" } ] },
            ]}
        }}
    });
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    path
}

/// Boot the code-mode tree over a recorded transcript, run ONE wake on it, and hand back the
/// kernel and the wake's steps.
async fn one_codemode_wake(program: &str) -> (Arc<Kernel>, TempDir, Vec<Step>) {
    let scratch = std::env::temp_dir().join(format!(
        "bough-codemode-inv-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let patch = transcript(&scratch, program);

    let (kernel, dir) = support::boot_real("codemode", &[patch]).await;

    let agents = kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: traj(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");
    kernel.quiesce().await;

    agent
        .followup(bough_plugin_agents::Message {
            id: bough_plugin_agents::MessageId::new(uuid_v7()),
            from: bough_plugin_agents::Sender::Andrey,
            subject: "read the readme".into(),
            text: "read the readme".into(),
            class: bough_plugin_agents::MailClass::Wake,
            refs: Default::default(),
            cites: Vec::new(),
            at: chrono::Utc::now(),
            mail_seq: None,
        })
        .await
        .expect("the message lands");
    agent.when_idle().await;
    disposer.dispose().await;
    kernel.quiesce().await;

    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound");
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            ..Default::default()
        })
        .await
        .expect("the chain reads back");
    let _ = std::fs::remove_dir_all(&scratch);
    (kernel, dir, steps)
}

/// The program every case runs: two host calls, so there are sub-steps to be wrong about.
const TWO_CALLS: &str =
    "console.log(await view(\"Cargo.toml\")); console.log(await view(\"Cargo.toml\"));";

fn kinds(steps: &[Step]) -> Vec<String> {
    steps.iter().map(|s| s.kind.to_string()).collect()
}

/// The gate: with a program in the chain, the runner reports nothing — including `agent-loop`'s
/// `every_request_reconstructs_from_the_ledger`, which is the one code mode could break.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_loop_invariant_passes_under_code_mode() {
    let _guard = trace::test_lock();
    let (kernel, _dir, steps) = one_codemode_wake(TWO_CALLS).await;

    // Precondition: this wake really did run a program with sub-steps. An invariant that passes
    // over an empty chain proves nothing.
    let k = kinds(&steps);
    assert!(
        k.iter().any(|x| x == "program/call"),
        "the wake must have run a program: {k:?}"
    );
    assert!(
        !sent_for(&steps).is_empty(),
        "the loop must have recorded the requests it sent"
    );

    kernel.run_invariants().await;
    assert!(
        kernel.violations().is_empty(),
        "code mode must violate nothing: {:#?}",
        kernel.violations()
    );
    kernel.shutdown().await;
}

/// The specific claim, said directly rather than through the runner: every request the loop
/// actually sent rebuilds from the steps, byte for byte.
#[tokio::test(flavor = "multi_thread")]
async fn the_request_reconstructs_byte_for_byte_under_code_mode() {
    let _guard = trace::test_lock();
    let (kernel, _dir, steps) = one_codemode_wake(TWO_CALLS).await;

    // The program TEXT is on the chain too: it is the `run` call's args, the model-visible half
    // of the pair, and it must be there verbatim or the reconstruction below cannot be honest.
    let run_call = steps
        .iter()
        .find(|s| s.kind.as_str() == "tool/call")
        .expect("the run call is a step");
    assert_eq!(
        run_call.body["args"]["program"]
            .as_str()
            .unwrap_or_default(),
        TWO_CALLS,
        "the program source is ledgered verbatim on the run call"
    );

    let sent = sent_for(&steps);
    assert!(
        sent.len() >= 2,
        "one round for the program, one for the answer"
    );
    bough_plugin_agent_loop::invariant::evaluate_reconstruction(&sent, &steps)
        .expect("every request the model saw must rebuild from the ledger");
    kernel.shutdown().await;
}

/// The half that is unique to this consumer. The sub-steps are ledgered and the model never saw
/// them as messages, so `transcript::rebuild` must skip them — and what it keeps for the `run`
/// call must be the CONSOLE, once. If a `program/*` kind ever started folding into the rebuild,
/// the model's second round would silently gain a transcript it was never sent.
#[tokio::test(flavor = "multi_thread")]
async fn inner_sub_steps_never_enter_the_reconstruction() {
    let _guard = trace::test_lock();
    let (kernel, _dir, steps) = one_codemode_wake(TWO_CALLS).await;

    let k = kinds(&steps);
    let subs = k.iter().filter(|x| x.starts_with("program/")).count();
    assert!(
        subs >= 4,
        "two calls, two results, and their console: {k:?}"
    );

    let rebuilt = bough_plugin_agent_loop::transcript::rebuild(&steps, None);
    let text = format!("{rebuilt:?}");

    // Exactly one tool-use/tool-result pair reaches the model: the `run` call.
    let with_only_run: Vec<&Step> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "tool/call" || s.kind.as_str() == "tool/result")
        .collect();
    assert_eq!(
        with_only_run.len(),
        2,
        "one API call and one API result: {:?}",
        with_only_run
            .iter()
            .map(|s| s.kind.as_str())
            .collect::<Vec<_>>()
    );

    // The sub-step CALL ids are `{run_call}.{n}` (plan D-5) and must appear nowhere in what the
    // model was shown. The `run` call id itself must, since that pair is model-visible.
    assert!(
        text.contains("c0"),
        "the `run` call must be in the reconstruction"
    );
    assert!(
        !text.contains("c0.0") && !text.contains("c0.1"),
        "an inner call id leaked into the reconstruction:\n{text}"
    );

    // And the model was shown the console once — not the sub-results a second time.
    let console: String = steps
        .iter()
        .filter(|s| s.kind.as_str() == "program/console")
        .filter_map(|s| s.body["text"].as_str())
        .collect();
    assert!(!console.is_empty(), "the program printed something");
    let result = steps
        .iter()
        .find(|s| s.kind.as_str() == "tool/result")
        .expect("the run result");
    assert_eq!(
        result.body["content"].as_str().unwrap_or_default(),
        console,
        "the console chunks must reassemble into exactly the result the model received (D-4)"
    );
    kernel.shutdown().await;
}
