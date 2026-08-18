//! The bundled `git-ai` plugin's hook, driven end to end: a real Luau
//! interpreter, a real git repository, and a fake `git-ai` on PATH that
//! records what it was handed.
//!
//! THE FAKE IS THE POINT. What matters about this hook is the SHAPE of what
//! reaches Git AI — the preset it invokes, the `type` on each payload, the
//! paths it claims the turn edited — and none of that is observable from the
//! hook's return value, which is empty by design. So `git-ai` here is a script
//! that appends its stdin to a file, and the assertions are about the file.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use bough_core::hooks::{HookDispatch, HookEvent, HookHost};

/// `PATH` is process-global and cargo runs these in parallel threads, so every
/// test that rewrites it takes the same lock.
fn path_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Run `f` with `PATH` set to `path`, restoring the old one even on a panic.
fn with_path<R>(path: &str, f: impl FnOnce() -> R) -> R {
    let _guard = path_lock();
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
    let _restore = Restore(std::env::var("PATH").ok());
    std::env::set_var("PATH", path);
    f()
}

/// A temp directory that removes itself.
struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmp(name: &str) -> Tmp {
    let dir = std::env::temp_dir().join(format!("bough-gitai-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Tmp(dir)
}

fn run(dir: &Path, program: &str, args: &[&str]) {
    let out = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"));
    assert!(out.status.success(), "{program} {args:?}: {out:?}");
}

/// A git repository with one committed file, so `status --porcelain` starts
/// clean and anything it reports later is this turn's doing.
fn repo(dir: &Path) {
    run(dir, "git", &["init", "-q"]);
    run(dir, "git", &["config", "user.email", "t@example.com"]);
    run(dir, "git", &["config", "user.name", "T"]);
    std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
    run(dir, "git", &["add", "-A"]);
    run(dir, "git", &["commit", "-qm", "first"]);
}

/// A `git-ai` on PATH that appends every invocation's argv and stdin to a log.
/// It is also reachable as `git ai …`, which is how the hook calls it — the
/// real binary installs itself as a `git` shim, so a fake that only answered to
/// `git-ai` would leave the checkpoint calls untested.
fn fake_git_ai(bin: &Path, log: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >> {log}\nprintf '\\n--\\n' >> {log}\n",
        log = log.display()
    );
    let git_ai = bin.join("git-ai");
    std::fs::write(&git_ai, &script).unwrap();
    // `git ai checkpoint …` has to reach the fake too, so `git` here forwards
    // an `ai` subcommand to it and everything else to the real git.
    let git = bin.join("git");
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\nif [ \"$1\" = ai ]; then shift; exec {} \"$@\"; fi\nexec {} \"$@\"\n",
            git_ai.display(),
            real_git()
        ),
    )
    .unwrap();
    for f in [&git_ai, &git] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

fn real_git() -> String {
    let out = std::process::Command::new("/usr/bin/which")
        .arg("git")
        .output()
        .expect("which git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The plugin's hook, loaded from the repository rather than from a
/// materialized copy: this is about the Lua that ships, so a broken edit fails
/// here rather than at someone's install.
fn host(dir: &Path) -> HookHost {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/git-ai/hooks/git-ai.lua"),
    )
    .expect("the bundled plugin's hook exists");
    let hooks = dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("git-ai.lua"), src).unwrap();
    HookHost::load(&hooks).expect("the hook loads")
}

fn dispatch(workspace: &Path, data: serde_json::Value) -> HookDispatch {
    HookDispatch {
        session_id: "conv-1".into(),
        workspace: workspace.to_string_lossy().into_owned(),
        pattern: "bash".into(),
        data,
    }
}

/// Every payload the fake was handed, in order.
fn payloads(log: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.split("\n--\n")
        .filter_map(|entry| {
            let (_argv, body) = entry.split_once('\n')?;
            serde_json::from_str(body.trim()).ok()
        })
        .collect()
}

fn argv(log: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.split("\n--\n")
        .filter(|e| !e.trim().is_empty())
        .filter_map(|e| e.split('\n').next().map(str::to_string))
        .collect()
}

/// The whole point of the plugin: a turn's edits reach Git AI as the agent's,
/// and whatever was on disk before it is marked as the human's first.
#[test]
fn a_turn_is_checkpointed_human_before_and_agent_after() {
    let dir = tmp("turn");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    repo(&ws);
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);

    with_path(&path, || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(
                &ws,
                serde_json::json!({ "prompt": "add a file", "model": "opus-5" }),
            ),
        );
        // The turn edits: one new file, one changed, one deleted.
        std::fs::write(ws.join("new.txt"), "hello\n").unwrap();
        std::fs::write(ws.join("kept.txt"), "one\ntwo\n").unwrap();
        host.dispatch(
            HookEvent::TurnEnd,
            dispatch(&ws, serde_json::json!({ "ok": true })),
        );
    });

    let calls = argv(&log);
    assert!(
        calls
            .iter()
            .all(|a| a.contains("checkpoint agent-v1 --hook-input stdin")),
        "the generic preset, because bough is not Claude Code: {calls:?}"
    );
    let seen = payloads(&log);
    assert_eq!(seen.len(), 2, "one before, one after: {seen:?}");

    let before = &seen[0];
    assert_eq!(before["type"], "human", "what was already there is yours");
    assert_eq!(before["repo_working_dir"], ws.to_string_lossy().as_ref());

    let after = &seen[1];
    assert_eq!(after["type"], "ai_agent");
    assert_eq!(after["agent_name"], "bough");
    assert_eq!(after["model"], "opus-5", "the model came from TurnStart");
    assert_eq!(after["conversation_id"], "conv-1");
    let edited: Vec<String> = after["edited_filepaths"]
        .as_array()
        .expect("edited_filepaths")
        .iter()
        .map(|p| {
            Path::new(p.as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(edited.contains(&"new.txt".to_string()), "{edited:?}");
    assert!(
        edited.contains(&"kept.txt".to_string()),
        "a file that was already committed and got edited: {edited:?}"
    );
    // The prompt rides along; nothing claims to be the assistant's words.
    let messages = after["transcript"]["messages"]
        .as_array()
        .expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["type"], "user");
    assert_eq!(messages[0]["text"], "add a file");
    assert!(messages[0]["timestamp"].is_string());
}

/// A turn that changed nothing has nothing to attribute, and a checkpoint over
/// an untouched repository is a diff for no reason.
#[test]
fn a_turn_that_edited_nothing_does_not_checkpoint_the_agent() {
    let dir = tmp("noop");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    repo(&ws);
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);

    with_path(&path, || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(
                &ws,
                serde_json::json!({ "prompt": "just look", "model": "m" }),
            ),
        );
        host.dispatch(
            HookEvent::TurnEnd,
            dispatch(&ws, serde_json::json!({ "ok": true })),
        );
    });

    let seen = payloads(&log);
    assert_eq!(seen.len(), 1, "the human checkpoint only: {seen:?}");
    assert_eq!(seen[0]["type"], "human");
}

/// A shell command can change files too — a `sed -i`, a formatter, a
/// generator — and Git AI takes the pair directly.
#[test]
fn a_shell_command_is_reported_as_a_pair() {
    let dir = tmp("shell");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    repo(&ws);
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);

    with_path(&path, || {
        host.dispatch(
            HookEvent::PreTool,
            dispatch(
                &ws,
                serde_json::json!({ "tool": "bash", "input": { "command": "sed -i s/a/b/ kept.txt" } }),
            ),
        );
        host.dispatch(
            HookEvent::PostTool,
            dispatch(
                &ws,
                serde_json::json!({ "tool": "bash", "command": "sed -i s/a/b/ kept.txt", "output": "" }),
            ),
        );
    });

    let seen = payloads(&log);
    let kinds: Vec<&str> = seen.iter().filter_map(|p| p["type"].as_str()).collect();
    assert_eq!(kinds, ["pre_shell_command", "post_shell_command"]);
    assert_eq!(seen[0]["command"], "sed -i s/a/b/ kept.txt");
    assert_eq!(seen[1]["agent_name"], "bough");
}

/// THE INERT CASE, which is most machines. No `git-ai` on PATH means no
/// subprocess, no payload and no error — a harness that failed a turn because
/// an attribution tool is missing would be worse than one that never had it.
#[test]
fn without_git_ai_installed_nothing_happens_and_nothing_fails() {
    let dir = tmp("absent");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    repo(&ws);
    let bin = dir.0.join("empty-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let host = host(&dir.0);

    // A PATH with no git-ai on it — and no git either, so a hook that shelled
    // out regardless would be visible as an error rather than as silence.
    let out = with_path(bin.to_string_lossy().as_ref(), || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(&ws, serde_json::json!({ "prompt": "hi", "model": "m" })),
        )
    });
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.is_empty(), "a hook with nothing to say says nothing");
}

/// A workspace that is not a git repository has nothing to attribute to
/// anyone, and `git ai checkpoint` outside one is an error per turn.
#[test]
fn outside_a_git_repository_the_hook_stays_out_of_the_way() {
    let dir = tmp("norepo");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);

    with_path(&path, || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(&ws, serde_json::json!({ "prompt": "hi", "model": "m" })),
        );
    });
    assert!(payloads(&log).is_empty(), "no repository, no checkpoint");
}
