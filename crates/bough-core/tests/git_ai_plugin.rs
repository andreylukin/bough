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

/// `x.pipe(f)` — reads better than nesting when a payload is built and then
/// wrapped in a dispatch.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

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
///
/// EVERY TEST HOLDS THIS FOR ITS WHOLE BODY, assertions included. `PATH` is
/// process-global and cargo runs these in parallel: locking only the dispatch
/// and then reading the log outside it let one test observe a window in which
/// another test's fake was the one on `PATH`, which showed up as a test that
/// passed alone and failed in the suite.
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
///
/// ONLY `git-ai`, no `git` shim. The hook invokes the binary directly rather
/// than through Git AI's `git ai` proxy, so nothing here has to stand in for
/// git — which also means the real git keeps working for the `rev-parse` and
/// `status` calls the hook makes, with no chance of a shim resolving to
/// another test's shim while `PATH` is shared.
fn fake_git_ai(bin: &Path, log: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >> {log}\nprintf '\\n--\\n' >> {log}\n",
        log = log.display()
    );
    let git_ai = bin.join("git-ai");
    std::fs::write(&git_ai, &script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&git_ai, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
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

/// `/var/…` and `/private/var/…` are the same directory on macOS, and
/// `rev-parse --show-toplevel` answers with the resolved one. Compare
/// directories through this, never as strings.
fn same_dir(a: &str, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
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
        assert!(same_dir(before["repo_working_dir"].as_str().unwrap(), &ws));

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
    });
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

        let seen = payloads(&log);
        assert_eq!(seen.len(), 1, "the human checkpoint only: {seen:?}");
        assert_eq!(seen[0]["type"], "human");
    });
}

/// Agent V1 only understands human and ai_agent checkpoints. A shell command
/// is therefore baselined immediately before it runs and attributed at TurnEnd,
/// rather than emitted as an unsupported pre/post shell event.
#[test]
fn a_shell_command_is_attributed_at_turn_end() {
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
            HookEvent::TurnStart,
            dispatch(
                &ws,
                serde_json::json!({ "prompt": "change it", "model": "m" }),
            ),
        );
        host.dispatch(
            HookEvent::PreTool,
            dispatch(
                &ws,
                serde_json::json!({ "tool": "bash", "input": { "command": "sed -i s/one/two/ kept.txt" } }),
            ),
        );
        std::fs::write(ws.join("kept.txt"), "two\n").unwrap();
        host.dispatch(
            HookEvent::TurnEnd,
            dispatch(&ws, serde_json::json!({ "ok": true })),
        );

        let seen = payloads(&log);
        let kinds: Vec<&str> = seen.iter().filter_map(|p| p["type"].as_str()).collect();
        assert_eq!(kinds, ["human", "ai_agent"], "{seen:?}");
        assert_eq!(seen[1]["agent_name"], "bough");
        assert_eq!(seen[1]["model"], "m");
        assert_eq!(seen[1]["transcript"]["messages"][0]["text"], "change it");
    });
}

/// Started in a SUBDIRECTORY of a repository, which is the normal case for a
/// workspace pointed at a crate inside a monorepo.
///
/// `git status --porcelain` reports paths relative to the REPO ROOT wherever it
/// is run from, so joining them to the workspace named files that do not exist
/// (`/repo/sub/sub/f.txt`) and Git AI was asked to attribute nothing.
#[test]
fn a_workspace_below_the_repo_root_names_the_files_that_actually_changed() {
    let dir = tmp("subdir");
    let ws = dir.0.join("ws");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    repo(&ws);
    std::fs::write(ws.join("sub/tracked.txt"), "one\n").unwrap();
    run(&ws, "git", &["add", "-A"]);
    run(&ws, "git", &["commit", "-qm", "sub"]);
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);
    let below = ws.join("sub");

    with_path(&path, || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(&below, serde_json::json!({ "prompt": "p", "model": "m" })),
        );
        std::fs::write(ws.join("sub/tracked.txt"), "one\ntwo\n").unwrap();
        host.dispatch(
            HookEvent::TurnEnd,
            dispatch(&below, serde_json::json!({ "ok": true })),
        );

        let seen = payloads(&log);
        assert_eq!(seen.len(), 2, "{seen:?}");
        for payload in &seen {
            assert!(
                same_dir(payload["repo_working_dir"].as_str().unwrap(), &ws),
                "the REPOSITORY, not the directory bough happens to be pointed at: {payload:?}"
            );
        }
        let edited: Vec<PathBuf> = seen[1]["edited_filepaths"]
            .as_array()
            .expect("edited_filepaths")
            .iter()
            .map(|p| PathBuf::from(p.as_str().unwrap()))
            .collect();
        assert_eq!(edited.len(), 1, "{edited:?}");
        assert!(
            edited[0].is_file(),
            "a path that does not exist attributes nothing: {edited:?}"
        );
        assert!(edited[0].ends_with("sub/tracked.txt"), "{edited:?}");
    });
}

/// A shell command can work in a repository outside the session workspace.
/// PreTool baselines that repository before the command, so TurnEnd can attribute
/// the command's changes without relying on unsupported shell checkpoint types.
#[test]
fn a_command_run_inside_a_repo_is_attributed_to_that_repo_not_the_workspace() {
    let dir = tmp("elsewhere");
    let home = dir.0.join("home");
    let ws = home.join("repos/project");
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
                &home,
                serde_json::json!({ "prompt": "fix it", "model": "m" }),
            ),
        );
        host.dispatch(
            HookEvent::PreTool,
            dispatch(
                &ws,
                serde_json::json!({ "tool": "bash", "input": { "command": "sed -i s/one/two/ kept.txt" } }),
            ),
        );
        std::fs::write(ws.join("kept.txt"), "two\n").unwrap();
        host.dispatch(
            HookEvent::TurnEnd,
            dispatch(&home, serde_json::json!({ "ok": true })),
        );

        let seen = payloads(&log);
        let kinds: Vec<&str> = seen.iter().filter_map(|p| p["type"].as_str()).collect();
        assert_eq!(kinds, ["human", "ai_agent"], "{seen:?}");
        for payload in &seen {
            assert!(same_dir(payload["repo_working_dir"].as_str().unwrap(), &ws));
        }
        let edited = seen[1]["edited_filepaths"].as_array().unwrap();
        assert_eq!(edited.len(), 1, "{seen:?}");
        assert!(
            edited[0].as_str().unwrap().ends_with("kept.txt"),
            "{seen:?}"
        );
    });
}

/// A first write in a repository outside the session workspace must be
/// baselined before it lands. Otherwise the only safe outcome was to drop the
/// turn, which also dropped its prompt transcript from Git AI.
#[test]
fn a_first_turn_file_write_outside_the_workspace_is_attributed() {
    let dir = tmp("home");
    let home = dir.0.join("home");
    let project = home.join("repos/project");
    std::fs::create_dir_all(&project).unwrap();
    repo(&project);
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);
    let edited = project.join("kept.txt");

    with_path(&path, || {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch(
                &home,
                serde_json::json!({ "prompt": "edit it", "model": "opus-5" }),
            ),
        );
        host.dispatch(
            HookEvent::Custom("PreWrite".into()),
            dispatch(&project, serde_json::json!({ "path": edited })),
        );
        std::fs::write(&edited, "one\ntwo\n").unwrap();
        host.dispatch(
            HookEvent::TurnEnd,
            serde_json::json!({ "ok": true, "edited": [edited.to_string_lossy()] })
                .pipe(|data| dispatch(&home, data)),
        );

        let seen = payloads(&log);
        let kinds: Vec<&str> = seen.iter().filter_map(|p| p["type"].as_str()).collect();
        assert_eq!(kinds, ["human", "ai_agent"], "{seen:?}");
        for payload in &seen {
            assert!(
                same_dir(payload["repo_working_dir"].as_str().unwrap(), &project),
                "{payload:?}"
            );
        }
        assert_eq!(seen[1]["transcript"]["messages"][0]["text"], "edit it");
        let paths = seen[1]["edited_filepaths"].as_array().unwrap();
        assert_eq!(paths.len(), 1, "{seen:?}");
        assert!(paths[0].as_str().unwrap().ends_with("kept.txt"), "{seen:?}");
    });
}

/// A turn that writes into TWO checkouts gets two checkpoints, one per
/// repository — the reason repositories are discovered from the work rather
/// than assumed to be the workspace.
#[test]
fn a_turn_that_touches_two_repos_checkpoints_each_of_them() {
    let dir = tmp("two");
    let home = dir.0.join("home");
    let a = home.join("repos/a");
    let b = home.join("repos/b");
    for p in [&a, &b] {
        std::fs::create_dir_all(p).unwrap();
        repo(p);
    }
    let log = dir.0.join("calls.log");
    let bin = dir.0.join("bin");
    fake_git_ai(&bin, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let host = host(&dir.0);

    with_path(&path, || {
        // Turn one discovers both; turn two attributes both.
        for round in 0..2 {
            host.dispatch(
                HookEvent::TurnStart,
                dispatch(&home, serde_json::json!({ "prompt": "go", "model": "m" })),
            );
            std::fs::write(a.join("kept.txt"), format!("a{round}\n")).unwrap();
            std::fs::write(b.join("kept.txt"), format!("b{round}\n")).unwrap();
            host.dispatch(
                HookEvent::TurnEnd,
                serde_json::json!({
                    "ok": true,
                    "edited": [
                        a.join("kept.txt").to_string_lossy(),
                        b.join("kept.txt").to_string_lossy(),
                    ]
                })
                .pipe(|data| dispatch(&home, data)),
            );
        }
        let seen = payloads(&log);
        let agent: Vec<&serde_json::Value> =
            seen.iter().filter(|p| p["type"] == "ai_agent").collect();
        assert_eq!(agent.len(), 2, "one per repository: {seen:?}");
        let mut dirs: Vec<&str> = agent
            .iter()
            .map(|p| p["repo_working_dir"].as_str().unwrap())
            .collect();
        dirs.sort();
        assert!(dirs[0].ends_with("/a"), "{dirs:?}");
        assert!(dirs[1].ends_with("/b"), "{dirs:?}");
    });
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
        assert!(payloads(&log).is_empty(), "no repository, no checkpoint");
    });
}
