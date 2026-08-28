//! V6 — a turn ends WITHOUT a stop tool.
//!
//! Main's code mode shipped two API tools, `run_steps(program)` and `stop`. This phase ships one:
//! the model ends its turn by answering in text, and the wake ends because the agent-loop's
//! wake-stopping listeners say it did (§5). A `stop` tool would be a second way to say the same
//! thing, and a model that forgot to call it would hang.
//!
//! These drive the REAL BINARY, because the thing under test is "does the turn end" — a property
//! of the process, not of a function. Each test writes its own recorded transcript, so the round
//! shapes are visible next to the assertion instead of in a shared fixture.
//!
//! The programs call `view`, not `bash`. That is not a preference: with `tags_required` on, no
//! registered tool has a `tags` property (`tools-baseline`'s `bash` is `{command, cwd}` and
//! `tools-operator` registers no `bash`/`sh`), so every shell call in the sandbox is refused
//! today. `docs/codemode-merge-notes.md` §9 records it. What these tests are about — the turn
//! ending with no `stop` tool — is independent of which host function the program called.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use bough_plugin_hello::trace;
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{Tools, ToolsHandle};

// ---------------------------------------------------------------------------------------------
// A code-mode `bough exec`, out of process.

/// A throwaway `$BOUGH_HOME` + `--root` + work tree. Removed on drop.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    /// `bough exec` forces `--profile headless` (`exec::force_profile`) and a `--patch` layer
    /// cannot CREATE a row (`config::patch`), so the codemode rows arrive the only way they can:
    /// a scratch `--root` whose `profiles/headless.yml` IS the shipped `profiles/codemode.yml`.
    /// `docs/codemode-merge-notes.md` §8 records why. Only the document's name changes.
    fn new(tag: &str) -> Sandbox {
        let home = std::env::temp_dir().join(format!(
            "bough-codemode-wake-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("work")).unwrap();
        // One file for the programs to view. The fixture is deliberately trivial: these tests
        // assert on how the TURN ends, not on what the program found.
        std::fs::write(home.join("work/README.md"), "one\ntwo\nthree\n").unwrap();
        let sb = Sandbox { home };
        sb.lay_out_root();
        sb
    }

    fn root(&self) -> PathBuf {
        self.home.join("root")
    }

    fn lay_out_root(&self) {
        let repo = support::repo_root();
        copy_tree(&repo.join("bundles"), &self.root().join("bundles"));
        copy_tree(&repo.join("profiles"), &self.root().join("profiles"));
        let text = std::fs::read_to_string(repo.join("profiles/codemode.yml")).unwrap();
        std::fs::write(
            self.root().join("profiles/headless.yml"),
            text.replace("name: codemode", "name: headless"),
        )
        .unwrap();
    }

    /// Write a recorded-transcript patch layer and return its path.
    fn transcript(&self, rounds: serde_json::Value) -> PathBuf {
        let path = self.home.join("transcript.yml");
        let doc = serde_json::json!({
            "entries": { "llm.anthropic": {
                "plugin": "llm-replay",
                "config": { "strict": true, "models": "*", "rounds": rounds }
            }}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    /// Run `bough exec` under the code-mode tree. Returns (exit code, stdout+stderr).
    fn exec(&self, task: &str, transcript: serde_json::Value) -> (i32, String) {
        let patch = self.transcript(transcript);
        let out = Command::new(env!("CARGO_BIN_EXE_bough"))
            .current_dir(self.home.join("work"))
            .env("BOUGH_HOME", &self.home)
            .env("HOME", &self.home)
            .arg("--root")
            .arg(self.root())
            .arg("--patch")
            .arg(&patch)
            .arg("exec")
            .arg(task)
            .output()
            .expect("the bough binary runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every step of the run, as `(kind, body)`, in ledger order.
    fn steps(&self) -> Vec<(String, serde_json::Value)> {
        let db = self.home.join("ledger.db");
        assert!(db.is_file(), "the run left no ledger at {db:?}");
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("the ledger opens read-only");
        let mut q = conn
            .prepare("SELECT type, body FROM steps ORDER BY traj_id, seq")
            .unwrap();
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?)
                        .unwrap_or(serde_json::Value::Null),
                ))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn kinds(&self) -> Vec<String> {
        self.steps().into_iter().map(|(k, _)| k).collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// One recorded round that calls `run` with `program`.
fn program_round(id: &str, program: &str) -> serde_json::Value {
    serde_json::json!({ "chunks": [
        { "type": "tool_call", "id": id, "name": "run", "input": { "program": program } },
        { "type": "usage", "input_tokens": 1000, "output_tokens": 100 },
        { "type": "end", "stop": "tool_use" },
    ]})
}

/// One recorded round that answers in text and calls nothing. THIS is how a turn ends.
fn answer_round(text: &str) -> serde_json::Value {
    serde_json::json!({ "chunks": [
        { "type": "text", "text": text },
        { "type": "usage", "input_tokens": 1000, "output_tokens": 20 },
        { "type": "end", "stop": "end_turn" },
    ]})
}

// ---------------------------------------------------------------------------------------------

/// The shape the phase is built on: program, then text, then the wake is over. No tool said so.
#[test]
fn a_program_then_text_wake_ends_by_wake_stopping() {
    let sb = Sandbox::new("ends");
    let (code, out) = sb.exec(
        "read the fixture readme",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            answer_round("three."),
        ]),
    );
    assert_eq!(code, 0, "the turn must end cleanly:\n{out}");
    assert!(
        out.contains("three."),
        "the answer must reach stdout:\n{out}"
    );

    let kinds = sb.kinds();
    assert!(kinds.contains(&"program/call".to_string()), "{kinds:?}");
    let end = sb
        .steps()
        .into_iter()
        .find(|(k, _)| k == "wake/end")
        .expect("the wake must end");
    assert_eq!(
        end.1["reason"], "completed",
        "the wake ended because the model stopped calling tools, not because it was cut off"
    );
    // And the text round called nothing at all: exactly one `run` call in the whole wake.
    assert_eq!(
        kinds.iter().filter(|k| *k == "tool/call").count(),
        1,
        "one API call for the program and none for the answer: {kinds:?}"
    );
}

/// A program with no host call is still a complete step: the `run` tool ran, its console came
/// back, and the round closed. Nothing waits for a call that never comes.
#[test]
fn a_program_that_calls_nothing_still_ends_its_step() {
    let sb = Sandbox::new("silent");
    let (code, out) = sb.exec(
        "say something",
        serde_json::json!([
            program_round("c0", "console.log(1 + 1)"),
            answer_round("two."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    assert_eq!(
        result.1["outcome"], "ok",
        "a call-free program is not an error"
    );
    assert!(
        result.1["content"]
            .as_str()
            .unwrap_or_default()
            .contains('2'),
        "the console is what comes back: {}",
        result.1["content"]
    );
    assert!(
        !steps.iter().any(|(k, _)| k == "program/call"),
        "a program that calls nothing appends no `program/call`"
    );
    assert_eq!(
        steps.iter().filter(|(k, _)| k == "step/end").count(),
        2,
        "both rounds closed their step"
    );
}

/// No `stop` tool anywhere, under EITHER surface. Main shipped one; this phase deliberately does
/// not, and a `stop` that crept back in would be a second, silent way to end a turn.
#[tokio::test(flavor = "multi_thread")]
async fn no_stop_tool_is_registered_by_either_consumer() {
    let _guard = trace::test_lock();
    for profile in ["headless", "codemode"] {
        let (kernel, _dir) = support::boot_real(profile, &[]).await;
        let tools = kernel
            .root()
            .peek_live::<Tools>()
            .expect("`tools` is bound") as std::sync::Arc<ToolsHandle>;
        let all: BTreeSet<String> = tools
            .visible(&AgentName::new("sol"))
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        for banned in ["stop", "run_steps", "end_turn"] {
            assert!(
                !all.contains(banned),
                "`{banned}` is registered under `{profile}`: {all:?}"
            );
        }
        kernel.shutdown().await;
    }
}

/// The failure this design has to rule out: a model that never signals the end. There is nothing
/// to signal, so the turn ends when the answer round does — and `exec` returns.
#[test]
fn a_wake_never_hangs_waiting_for_a_stop() {
    let sb = Sandbox::new("nohang");
    let started = std::time::Instant::now();
    let (code, out) = sb.exec(
        "do two programs then answer",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            program_round("c1", "console.log(await view(\"README.md\"))"),
            answer_round("done."),
        ]),
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(120),
        "the turn took {:?}; a turn that waits for a stop tool never returns",
        started.elapsed()
    );
    let kinds = sb.kinds();
    assert_eq!(
        kinds.iter().filter(|k| *k == "tool/call").count(),
        2,
        "two programs, then the answer: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "wake/end").count(),
        1,
        "the wake ended exactly once: {kinds:?}"
    );
}

/// §5: "a tool result carrying `concludes_wake` ends the wake at its step." Under code mode the
/// carrier is `run`, and `run` never invents the flag — it repeats what an inner result said.
///
/// The two halves are pinned where each is decidable:
/// `plugins/tools-codemode/tests/pipeline.rs::run_never_reports_concludes_wake_unless_an_inner_result_did`
/// (the program propagates it, both ways) and
/// `plugins/agent-loop/tests/flow.rs::a_concludes_wake_tool_result_ends_the_wake_at_its_step`
/// (the loop honours it). What is decidable HERE, on the real binary, is the negative half that
/// the two compose correctly: a program whose inner results do not conclude does NOT end the wake
/// at the program's step — the loop goes round again and the model gets to answer.
#[test]
fn a_concluding_inner_result_ends_the_wake_at_its_step() {
    let sb = Sandbox::new("conclude");
    let (code, out) = sb.exec(
        "run and then answer",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            answer_round("hi back."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let run_result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    assert_eq!(
        run_result.1["concludes_wake"], false,
        "no inner result concluded, so `run` must not claim one did"
    );
    // Every inner result agrees, which is what `run` is repeating.
    for (_, body) in steps.iter().filter(|(k, _)| k == "program/result") {
        assert_eq!(body["concludes_wake"], false, "inner: {body}");
    }
    // And because it did not conclude, the wake continued past the program's step.
    let kinds = sb.kinds();
    let program_step = kinds.iter().position(|k| k == "tool/result").unwrap();
    let wake_end = kinds.iter().position(|k| k == "wake/end").unwrap();
    assert!(
        program_step < wake_end,
        "the wake ended before the program's result — the run did not get that far: {kinds:?}\n{out}"
    );
    assert!(
        kinds[program_step..wake_end].contains(&"thought/text".to_string()),
        "the loop must have gone round again and let the model answer: {kinds:?}"
    );
}
