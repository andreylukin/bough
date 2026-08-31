//! Shared out-of-process harness for the code-mode tests (`codemode_wake.rs`, `codemode_closed.rs`).
//!
//! It runs the REAL `bough` binary under the shipped code-mode rows with a recorded transcript,
//! and reads the ledger the run left behind. Nothing here is a mock: the JS is executed by the
//! `js-quickjs` engine, every host call goes through the real tools pipeline, and the steps are
//! the ones the run actually appended.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway `$BOUGH_HOME` + `--root` + work tree. Removed on drop.
pub struct Sandbox {
    pub home: PathBuf,
}

impl Sandbox {
    /// `bough exec` forces `--profile headless` (`exec::force_profile`) and a `--patch` layer
    /// cannot CREATE a row (`config::patch`), so the codemode rows arrive the only way they can:
    /// a scratch `--root` whose `profiles/headless.yml` IS the shipped `profiles/codemode.yml`.
    /// `docs/codemode-merge-notes.md` §8 records why. Only the document's name changes.
    pub fn new(tag: &str) -> Sandbox {
        Sandbox::build(tag, true)
    }

    /// The same scratch tree with the SHIPPED `headless` profile: the TYPED consumer.
    ///
    /// The two constructors differ by one document, which is what makes a case that runs on both
    /// a comparison of SURFACES rather than of trees.
    pub fn typed(tag: &str) -> Sandbox {
        Sandbox::build(tag, false)
    }

    fn build(tag: &str, codemode: bool) -> Sandbox {
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
        sb.lay_out_root_for(codemode);
        sb
    }

    pub fn root(&self) -> PathBuf {
        self.home.join("root")
    }

    pub fn lay_out_root(&self) {
        self.lay_out_root_for(true);
    }

    pub fn lay_out_root_for(&self, codemode: bool) {
        let repo = super::repo_root();
        copy_tree(&repo.join("bundles"), &self.root().join("bundles"));
        copy_tree(&repo.join("profiles"), &self.root().join("profiles"));
        if codemode {
            // The SHIPPED `headless` profile is code mode since 2026-08-28; nothing to do.
            return;
        }
        // The typed arm is the one that now needs a document: `bough-typed` last, which is the
        // fallback `docs/configuration.md` names.
        std::fs::write(
            self.root().join("profiles/headless.yml"),
            "name: headless\nbundles: [bough-base, bough-headless, bough-typed]\ninvariants: false\n",
        )
        .unwrap();
    }

    /// Write a recorded-transcript patch layer and return its path.
    pub fn transcript(&self, rounds: serde_json::Value) -> PathBuf {
        self.transcript_with(rounds, serde_json::json!({}))
    }

    /// The same layer with FURTHER row entries merged in — a `gh_bin` pointed at a shim, say.
    pub fn transcript_with(&self, rounds: serde_json::Value, extra: serde_json::Value) -> PathBuf {
        let path = self.home.join("transcript.yml");
        let mut entries = serde_json::json!({ "llm.anthropic": {
            "plugin": "llm-replay",
            "config": { "strict": true, "models": "*", "rounds": rounds }
        }});
        if let (Some(dst), Some(src)) = (entries.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let doc = serde_json::json!({ "entries": entries });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    /// Run `bough exec` under the code-mode tree. Returns (exit code, stdout+stderr).
    pub fn exec(&self, task: &str, transcript: serde_json::Value) -> (i32, String) {
        self.exec_with(task, transcript, serde_json::json!({}), &[])
    }

    /// `exec` with extra patch entries and extra environment for the child.
    ///
    /// The environment is the child's alone — nothing here touches this process's, so cases using
    /// it need no `test_lock`.
    pub fn exec_with(
        &self,
        task: &str,
        transcript: serde_json::Value,
        extra: serde_json::Value,
        env: &[(&str, String)],
    ) -> (i32, String) {
        let patch = self.transcript_with(transcript, extra);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_bough"));
        // A diagnostic passthrough: `BOUGH_TEST_LOG=bough_kernel=debug cargo nextest run …` puts
        // the child's kernel log into the failure message. The child is otherwise quiet, which is
        // what made an intermittent boot race take three sessions to read.
        if let Ok(level) = std::env::var("BOUGH_TEST_LOG") {
            cmd.env("RUST_LOG", level);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd
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
    pub fn steps(&self) -> Vec<(String, serde_json::Value)> {
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

    pub fn kinds(&self) -> Vec<String> {
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
pub fn program_round(id: &str, program: &str) -> serde_json::Value {
    serde_json::json!({ "chunks": [
        { "type": "tool_call", "id": id, "name": "run", "input": { "program": program } },
        { "type": "usage", "input_tokens": 1000, "output_tokens": 100 },
        { "type": "end", "stop": "tool_use" },
    ]})
}

/// One recorded round that answers in text and calls nothing. THIS is how a turn ends.
pub fn answer_round(text: &str) -> serde_json::Value {
    serde_json::json!({ "chunks": [
        { "type": "text", "text": text },
        { "type": "usage", "input_tokens": 1000, "output_tokens": 20 },
        { "type": "end", "stop": "end_turn" },
    ]})
}

/// One recorded round that calls a TYPED tool by name.
pub fn tool_round(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "chunks": [
        { "type": "tool_call", "id": id, "name": name, "input": input },
        { "type": "usage", "input_tokens": 1000, "output_tokens": 100 },
        { "type": "end", "stop": "tool_use" },
    ]})
}
