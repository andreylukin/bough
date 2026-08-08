//! The hook contract, driven through a real Luau interpreter over real files
//! in a temp directory — the same path production takes, minus `~/.bough`.

use super::*;
use std::path::PathBuf;

struct Hooks {
    dir: PathBuf,
}

impl Hooks {
    /// A hooks directory holding `(name, source)` files.
    fn new(files: &[(&str, &str)]) -> Hooks {
        let dir = std::env::temp_dir().join(format!("bough-hooks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, src) in files {
            std::fs::write(dir.join(name), src).unwrap();
        }
        Hooks { dir }
    }

    fn host(&self) -> HookHost {
        HookHost::load(&self.dir).expect("a directory with lua in it loads")
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn dispatch(pattern: &str, data: serde_json::Value) -> HookDispatch {
    HookDispatch {
        session_id: "s1".into(),
        workspace: String::new(),
        pattern: pattern.into(),
        data,
    }
}

fn in_workspace(pattern: &str, workspace: &Path, data: serde_json::Value) -> HookDispatch {
    HookDispatch {
        workspace: workspace.to_string_lossy().into_owned(),
        ..dispatch(pattern, data)
    }
}

/// The bundled adapters, loaded from the repository rather than from a
/// materialized copy: these tests are about the Lua that ships, and reading it
/// where it lives means a broken edit fails here rather than at someone's
/// install.
fn bundled(name: &str) -> Hooks {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("hooks")
            .join(name),
    )
    .expect("the bundled hook exists");
    Hooks::new(&[(name, &src)])
}

/// A workspace with files in it. Paths are relative; parents are created.
fn workspace(files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bough-ws-{}", uuid::Uuid::new_v4()));
    for (rel, body) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn no_directory_and_no_lua_both_mean_no_host_and_no_thread() {
    let missing = std::env::temp_dir().join(format!("bough-absent-{}", uuid::Uuid::new_v4()));
    assert!(HookHost::load(&missing).is_none());

    let empty = Hooks::new(&[("notes.md", "not lua")]);
    assert!(
        HookHost::load(&empty.dir).is_none(),
        "a directory with no .lua in it is the same as no directory"
    );
}

#[test]
fn an_autocmd_fires_for_its_event_and_can_add_context() {
    let h = Hooks::new(&[(
        "ctx.lua",
        r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function(ev)
            bough.context("the branch is " .. ev.data.branch)
          end,
        })
        "#,
    )]);
    let host = h.host();
    assert_eq!(host.autocmd_count(), 1);

    let out = host.dispatch(
        HookEvent::TurnStart,
        dispatch("s1", serde_json::json!({ "branch": "main" })),
    );
    assert_eq!(out.context, ["the branch is main"]);
    assert!(out.errors.is_empty(), "{:?}", out.errors);

    // A different event does not reach it.
    let other = host.dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    assert!(other.is_empty(), "{other:?}");
}

#[test]
fn a_pretool_hook_denies_a_command_and_the_reason_travels() {
    let h = Hooks::new(&[(
        "guard.lua",
        r#"
        bough.api.create_autocmd("PreTool", {
          pattern = "bash",
          callback = function(ev)
            if string.find(ev.data.input.command, "rm %-rf /") then
              return { decision = "deny", reason = "not that one" }
            end
          end,
        })
        "#,
    )]);
    let host = h.host();

    let denied = host.dispatch(
        HookEvent::PreTool,
        dispatch(
            "bash",
            serde_json::json!({ "input": { "command": "rm -rf /" } }),
        ),
    );
    assert_eq!(denied.decision, Some(ToolDecision::Deny));
    assert_eq!(denied.reason.as_deref(), Some("not that one"));

    let allowed = host.dispatch(
        HookEvent::PreTool,
        dispatch("bash", serde_json::json!({ "input": { "command": "ls" } })),
    );
    assert_eq!(
        allowed.decision, None,
        "a hook that returns nothing decides nothing"
    );
}

#[test]
fn a_hook_rewrites_a_tool_input_and_a_tool_output() {
    let h = Hooks::new(&[(
        "rewrite.lua",
        r#"
        bough.api.create_autocmd("PreTool", {
          callback = function(ev)
            return { input = { command = ev.data.input.command .. " --color=never" } }
          end,
        })
        bough.api.create_autocmd("PostTool", {
          callback = function(ev)
            return { output = string.gsub(ev.data.output, "sk%-%w+", "[redacted]") }
          end,
        })
        "#,
    )]);
    let host = h.host();

    let pre = host.dispatch(
        HookEvent::PreTool,
        dispatch(
            "bash",
            serde_json::json!({ "input": { "command": "git diff" } }),
        ),
    );
    assert_eq!(
        pre.input,
        Some(serde_json::json!({ "command": "git diff --color=never" }))
    );

    let post = host.dispatch(
        HookEvent::PostTool,
        dispatch(
            "bash",
            serde_json::json!({ "output": "token sk-abc123 leaked" }),
        ),
    );
    assert_eq!(post.output.as_deref(), Some("token [redacted] leaked"));
}

#[test]
fn a_hook_can_inject_a_user_turn_and_rename_the_session() {
    let h = Hooks::new(&[(
        "followup.lua",
        r#"
        bough.api.create_autocmd("TurnEnd", {
          callback = function(ev)
            if ev.data.tests_failed then
              bough.session.prompt("the tests failed — fix them")
              bough.session.set_title("fixing the tests")
            end
          end,
        })
        "#,
    )]);
    let host = h.host();
    let out = host.dispatch(
        HookEvent::TurnEnd,
        dispatch("s1", serde_json::json!({ "tests_failed": true })),
    );
    assert_eq!(
        out.effects,
        vec![
            Effect::Prompt {
                text: "the tests failed — fix them".into()
            },
            Effect::SetTitle {
                title: "fixing the tests".into()
            },
        ],
        "session changes are recorded for the caller, in order"
    );
}

#[test]
fn a_blank_prompt_is_refused_as_a_programmer_mistake_and_costs_only_that_hook() {
    let h = Hooks::new(&[(
        "blank.lua",
        r#"
        bough.api.create_autocmd("TurnEnd", {
          callback = function() bough.session.prompt("   ") end,
        })
        bough.api.create_autocmd("TurnEnd", {
          callback = function() bough.context("the other hook still ran") end,
        })
        "#,
    )]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    assert!(out.effects.is_empty());
    assert_eq!(out.context, ["the other hook still ran"]);
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert!(out.errors[0].contains("blank"), "{:?}", out.errors);
}

#[test]
fn a_hook_that_throws_is_a_reported_error_not_a_failed_dispatch() {
    let h = Hooks::new(&[
        (
            "a-broken.lua",
            r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function() error("boom") end,
        })
        "#,
        ),
        (
            "b-fine.lua",
            r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function() bough.context("still here") end,
        })
        "#,
        ),
    ]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})));
    assert_eq!(
        out.context,
        ["still here"],
        "one bad hook does not silence the rest"
    );
    assert_eq!(out.errors.len(), 1);
    assert!(out.errors[0].contains("boom"), "{:?}", out.errors);
}

#[test]
fn a_file_that_does_not_parse_is_reported_and_the_others_still_load() {
    let h = Hooks::new(&[
        ("a-bad.lua", "this is not lua ((("),
        (
            "b-good.lua",
            r#"bough.api.create_autocmd("TurnEnd", { callback = function() end })"#,
        ),
    ]);
    let host = h.host();
    assert_eq!(host.loaded.len(), 1, "{:?}", host.loaded);
    assert_eq!(host.failed.len(), 1, "{:?}", host.failed);
    assert!(host.failed[0].0.ends_with("a-bad.lua"));
    assert_eq!(host.autocmd_count(), 1);
}

#[test]
fn a_runaway_hook_is_interrupted_and_the_dispatch_still_answers() {
    let h = Hooks::new(&[(
        "loop.lua",
        r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function() while true do end end,
        })
        "#,
    )]);
    let started = std::time::Instant::now();
    let out = h
        .host()
        .dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})));
    assert!(
        started.elapsed() < DISPATCH_TIMEOUT * 3,
        "the interrupt has to fire: took {:?}",
        started.elapsed()
    );
    assert!(out.context.is_empty());
}

#[test]
fn stop_is_sticky_and_the_first_reason_is_the_one_reported() {
    let h = Hooks::new(&[
        (
            "a.lua",
            r#"
        bough.api.create_autocmd("TurnEnd", { callback = function() bough.stop("first") end })
        "#,
        ),
        (
            "b.lua",
            r#"
        bough.api.create_autocmd("TurnEnd", { callback = function() return {} end })
        "#,
        ),
    ]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    assert_eq!(out.stop.as_deref(), Some("first"));
}

#[test]
fn a_pattern_narrows_to_one_tool_and_a_star_matches_everything() {
    let h = Hooks::new(&[(
        "patterns.lua",
        r#"
        bough.api.create_autocmd("PreTool", {
          pattern = "bash",
          callback = function() bough.context("bash only") end,
        })
        bough.api.create_autocmd("PreTool", {
          pattern = "*",
          callback = function() bough.context("everything") end,
        })
        "#,
    )]);
    let host = h.host();
    let bash = host.dispatch(HookEvent::PreTool, dispatch("bash", serde_json::json!({})));
    assert_eq!(bash.context, ["bash only", "everything"]);
    let view = host.dispatch(HookEvent::PreTool, dispatch("view", serde_json::json!({})));
    assert_eq!(view.context, ["everything"]);
}

#[test]
fn once_fires_exactly_once_and_del_autocmd_removes_a_listener() {
    let h = Hooks::new(&[(
        "once.lua",
        r#"
        bough.api.create_autocmd("TurnStart", {
          once = true,
          callback = function() bough.context("greeting") end,
        })
        local id = bough.api.create_autocmd("TurnEnd", {
          callback = function() bough.context("never") end,
        })
        bough.api.del_autocmd(id)
        "#,
    )]);
    let host = h.host();
    assert_eq!(host.autocmd_count(), 1, "del_autocmd removed the second");
    assert_eq!(
        host.dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})))
            .context,
        ["greeting"]
    );
    assert!(host
        .dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})))
        .is_empty());
    assert_eq!(host.autocmd_count(), 0);
}

#[test]
fn a_plugin_can_define_and_fire_its_own_event() {
    let h = Hooks::new(&[(
        "custom.lua",
        r#"
        bough.api.create_autocmd("Deployed", {
          callback = function(ev) bough.context("deployed " .. ev.data.env) end,
        })
        bough.api.create_autocmd("TurnEnd", {
          callback = function()
            bough.api.exec_autocmds("Deployed", { data = { env = "staging" } })
          end,
        })
        "#,
    )]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    assert_eq!(
        out.context,
        ["deployed staging"],
        "a plugin event reaches its own listeners through the host's own path"
    );
}

#[test]
fn one_call_can_listen_for_several_events_under_one_id() {
    let h = Hooks::new(&[(
        "multi.lua",
        r#"
        bough.api.create_autocmd({"TurnStart", "TurnEnd"}, {
          callback = function(ev) bough.context("saw " .. ev.event) end,
        })
        "#,
    )]);
    let host = h.host();
    assert_eq!(host.autocmd_count(), 2, "one listener per event");
    assert_eq!(
        host.dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})))
            .context,
        ["saw TurnStart"]
    );
    assert_eq!(
        host.dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})))
            .context,
        ["saw TurnEnd"]
    );
}

#[test]
fn an_outcome_names_the_verbs_it_used_so_the_tui_can_say_what_happened() {
    let h = Hooks::new(&[(
        "busy.lua",
        r#"
        bough.api.create_autocmd("PreTool", {
          callback = function(ev)
            bough.context("fyi")
            bough.session.prompt("and another thing")
            return { decision = "deny", reason = "no" }
          end,
        })
        "#,
    )]);
    let out = h.host().dispatch(
        HookEvent::PreTool,
        dispatch("bash", serde_json::json!({ "input": { "command": "ls" } })),
    );
    assert_eq!(
        out.verbs(),
        ["added context", "denied a command", "sent a prompt"],
        "the announcement names what was done, never what was said"
    );
}

#[test]
fn activity_is_attributed_to_the_file_that_did_it() {
    let h = Hooks::new(&[
        (
            "a-quiet.lua",
            r#"
        bough.api.create_autocmd("TurnEnd", { callback = function() end })
        "#,
        ),
        (
            "b-busy.lua",
            r#"
        bough.api.create_autocmd("TurnEnd", {
          callback = function() bough.session.set_title("renamed") end,
        })
        "#,
        ),
    ]);
    let host = h.host();
    host.dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    host.dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));

    let activity = host.activity();
    let busy = activity
        .iter()
        .find(|(path, _)| path.ends_with("b-busy.lua"))
        .expect("the busy hook has activity");
    assert_eq!(busy.1 .0, 2, "twice");
    assert_eq!(busy.1 .1.as_deref(), Some("renamed the session"));
    // A hook that RAN and chose to do nothing still counts as used: the one
    // worth finding is the one wired to an event that never fires, and only a
    // run count tells them apart.
    let quiet = activity
        .iter()
        .find(|(path, _)| path.ends_with("a-quiet.lua"))
        .expect("the quiet hook ran too");
    assert_eq!(quiet.1 .0, 2);
    assert_eq!(quiet.1 .1.as_deref(), Some("ran"));
}

#[test]
fn a_hook_that_only_throws_still_counts_as_having_acted() {
    let h = Hooks::new(&[(
        "boom.lua",
        r#"
        bough.api.create_autocmd("TurnEnd", { callback = function() error("nope") end })
        "#,
    )]);
    let host = h.host();
    host.dispatch(HookEvent::TurnEnd, dispatch("s1", serde_json::json!({})));
    let activity = host.activity();
    let (_, (fired, last)) = activity.iter().next().expect("one entry");
    assert_eq!(*fired, 1);
    assert_eq!(
        last.as_deref(),
        Some("failed"),
        "a hook failing every turn must be visible, not silent"
    );
}

#[test]
fn json_and_fs_follow_the_value_err_pair_convention() {
    let h = Hooks::new(&[(
        "util.lua",
        r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function()
            local decoded = bough.json.decode('{"n":3}')
            bough.context("n=" .. tostring(decoded.n))
            local text, err = bough.fs.read("/definitely/not/here")
            if err then bough.context("read failed as a value, not a throw") end
          end,
        })
        "#,
    )]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})));
    assert_eq!(
        out.context,
        ["n=3", "read failed as a value, not a throw"],
        "{:?}",
        out.errors
    );
}

// ---------------------------------------------------------------------------
// The bundled adapters. Driven over real files, through the real interpreter —
// what is asserted is what a Claude Code or Codex user would actually get.
// ---------------------------------------------------------------------------

#[test]
fn the_claude_code_adapter_injects_the_rules_directory_and_never_claude_md() {
    let ws = workspace(&[
        ("CLAUDE.md", "project rules here"),
        (".claude/rules/style.md", "two spaces"),
        (".claude/rules/notes.txt", "not markdown"),
    ]);
    let out = bundled("claude-code.lua").host().dispatch(
        HookEvent::TurnStart,
        in_workspace("s1", &ws, serde_json::json!({ "prompt": "hi" })),
    );
    let context = out.context.join("\n");
    assert!(context.contains("two spaces"), "{context}");
    assert!(
        !context.contains("not markdown"),
        "only .md files are rules: {context}"
    );
    // Each file is labelled, because a model handed four merged documents with
    // no headings cannot tell which rule came from where.
    assert!(context.contains(".claude/rules/style.md"), "{context}");
    // THE DOUBLE-INJECTION GATE. `prompt/project.rs` now reads CLAUDE.md
    // natively as a per-directory fallback, and it runs on every turn whether
    // or not this hook is on. If this hook injected it too, a CC-only repo
    // would carry its own rules twice in one prompt — once as a project rule
    // and once as hook context.
    assert!(
        !context.contains("project rules here"),
        "CLAUDE.md belongs to prompt/project.rs now, not to this hook: {context}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The adapters are the one exception to "bundled hooks are off": a machine
/// with no `.claude`/`.codex` config gets nothing from them, and a machine
/// with one was otherwise having its guardrails silently ignored.
#[test]
fn the_two_adapters_are_on_without_being_asked_for_and_no_other_bundled_hook_is() {
    let state = HookState::default();
    for id in sources::DEFAULT_ON {
        assert!(
            is_on(&state, id, SourceKind::Bundled),
            "{id} should adapt an existing config without being switched on"
        );
    }
    assert!(
        !is_on(&state, "bundled/guard-destructive.lua", SourceKind::Bundled),
        "a bundled hook with behaviour of its own still waits to be asked"
    );
    assert!(
        !is_on(&state, "someone-repo/claude-code.lua", SourceKind::Git),
        "the default-on list is keyed on the full id, so a cloned repo cannot \
         claim it by shipping the same file name"
    );
    // And an explicit off still wins, so turning one off survives this list.
    let off = HookState {
        off: vec!["bundled/claude-code.lua".into()],
        ..Default::default()
    };
    assert!(!is_on(&off, "bundled/claude-code.lua", SourceKind::Bundled));
}

#[test]
fn a_claude_code_pretooluse_hook_blocks_with_exit_2_and_its_stderr_is_the_reason() {
    let ws = workspace(&[(
        ".claude/settings.json",
        r#"{ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [
             { "type": "command", "command": "echo 'no rm here' >&2; exit 2" }
           ] } ] } }"#,
    )]);
    let out = bundled("claude-code.lua").host().dispatch(
        HookEvent::PreTool,
        in_workspace(
            "bash",
            &ws,
            serde_json::json!({ "input": { "command": "rm -rf /" } }),
        ),
    );
    assert_eq!(out.decision, Some(ToolDecision::Deny), "{out:?}");
    assert!(
        out.reason
            .as_deref()
            .is_some_and(|r| r.contains("no rm here")),
        "the hook's stderr is what the model reads: {:?}",
        out.reason
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_claude_code_hook_can_rewrite_the_command_through_its_own_json_shape() {
    let ws = workspace(&[(
        ".claude/settings.json",
        r#"{ "hooks": { "PreToolUse": [ { "hooks": [ { "type": "command",
             "command": "echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"command\":\"ls --color=never\"},\"additionalContext\":\"rewritten by a hook\"}}'"
           } ] } ] } }"#,
    )]);
    let out = bundled("claude-code.lua").host().dispatch(
        HookEvent::PreTool,
        in_workspace(
            "bash",
            &ws,
            serde_json::json!({ "input": { "command": "ls" } }),
        ),
    );
    assert_eq!(
        out.input,
        Some(serde_json::json!({ "command": "ls --color=never" })),
        "{out:?}"
    );
    assert_eq!(out.context, ["rewritten by a hook"]);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_claude_code_matcher_that_names_another_tool_does_not_fire_on_bash() {
    let ws = workspace(&[(
        ".claude/settings.json",
        r#"{ "hooks": { "PreToolUse": [ { "matcher": "Write", "hooks": [
             { "type": "command", "command": "exit 2" }
           ] } ] } }"#,
    )]);
    let out = bundled("claude-code.lua").host().dispatch(
        HookEvent::PreTool,
        in_workspace(
            "bash",
            &ws,
            serde_json::json!({ "input": { "command": "ls" } }),
        ),
    );
    assert_eq!(
        out.decision, None,
        "a Write matcher is not a Bash hook: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_missing_or_malformed_settings_file_costs_nothing() {
    let ws = workspace(&[(".claude/settings.json", "{ not json")]);
    let out = bundled("claude-code.lua").host().dispatch(
        HookEvent::PreTool,
        in_workspace(
            "bash",
            &ws,
            serde_json::json!({ "input": { "command": "ls" } }),
        ),
    );
    assert!(out.decision.is_none());
    assert!(
        out.errors.is_empty(),
        "a broken settings file is a warning, not a failed dispatch: {:?}",
        out.errors
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// A plugin's `plugin.json` `hooks` key is a POINTER to the hooks file, never
/// the hooks themselves — the shape real installed plugins ship, and the one
/// this adapter used to read as though it were an inline block, finding
/// nothing and saying nothing.
#[test]
fn an_installed_plugins_manifest_declared_hooks_file_is_run() {
    let home = workspace(&[]);
    let install = home.join("plug");
    std::fs::create_dir_all(install.join(".claude-plugin/hooks")).unwrap();
    std::fs::write(
        install.join(".claude-plugin/plugin.json"),
        serde_json::json!({"name": "p", "hooks": "./.claude-plugin/hooks/hooks.json"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        install.join(".claude-plugin/hooks/hooks.json"),
        serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    // `${CLAUDE_PLUGIN_ROOT}` is expanded to the install dir,
                    // the way Claude Code expands it when it spawns.
                    "hooks": [{
                        "type": "command",
                        "command": "echo ${CLAUDE_PLUGIN_ROOT} >&2; exit 2",
                    }],
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
    std::fs::write(
        home.join(".claude/plugins/installed_plugins.json"),
        serde_json::json!({
            "plugins": {"p@m": [{"installPath": install.to_string_lossy(), "scope": "user"}]}
        })
        .to_string(),
    )
    .unwrap();

    let ws = workspace(&[]);
    let hooks = bundled("claude-code.lua");
    let out = crate::paths::test_env::with_env(&[("HOME", home.to_str())], || {
        hooks.host().dispatch(
            HookEvent::PreTool,
            in_workspace(
                "bash",
                &ws,
                serde_json::json!({ "input": { "command": "ls" } }),
            ),
        )
    });
    assert_eq!(out.decision, Some(ToolDecision::Deny), "{out:?}");
    assert!(
        out.reason
            .as_deref()
            .unwrap_or_default()
            .contains(&install.to_string_lossy().to_string()),
        "the plugin root reached the command: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&ws);
}

/// Codex's chain runs repo-root DOWN TO the cwd, so the directories it reads
/// are the workspace's ancestors. The adapter once read the workspace's
/// CHILDREN instead, which put every sibling package's rules in every turn.
#[test]
fn the_codex_adapter_walks_up_to_the_git_root_and_never_into_siblings() {
    let root = workspace(&[
        (".git/HEAD", "ref: refs/heads/main"),
        ("AGENTS.override.md", "the repo override"),
        ("web/AGENTS.override.md", "the web override"),
        (
            "api/AGENTS.override.md",
            "a sibling that is not on the path",
        ),
        ("web/AGENTS.md", "the plain file bough already reads"),
    ]);
    let ws = root.join("web");
    let out = bundled("codex.lua").host().dispatch(
        HookEvent::TurnStart,
        in_workspace("s1", &ws, serde_json::json!({ "prompt": "hi" })),
    );
    let context = out.context.join("\n");
    assert!(context.contains("the repo override"), "{context}");
    assert!(context.contains("the web override"), "{context}");
    assert!(
        context.find("the repo override") < context.find("the web override"),
        "root first, so the nearest file has the last word: {context}"
    );
    assert!(
        !context.contains("a sibling that is not on the path"),
        "a sibling package's rules are not this workspace's: {context}"
    );
    assert!(
        !context.contains("the plain file bough already reads"),
        "AGENTS.md is native; repeating it is not emphasis: {context}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_codex_notify_array_is_parsed_out_of_config_toml_and_nothing_else_is() {
    // The parse is the risky part — a wrong read here would run the wrong
    // program — so it is pinned directly through a hook that echoes it back.
    let h = Hooks::new(&[(
        "probe.lua",
        r#"
        local function notify_argv(text)
          if text == nil then return nil end
          local line = string.match(text, "\n%s*notify%s*=%s*(%b[])")
            or string.match(text, "^%s*notify%s*=%s*(%b[])")
          if line == nil then return nil end
          local argv = {}
          for item in string.gmatch(line, '"([^"]*)"') do table.insert(argv, item) end
          if #argv == 0 then return nil end
          return argv
        end
        bough.api.create_autocmd("TurnStart", {
          callback = function(ev)
            local argv = notify_argv(ev.data.toml)
            bough.context(argv and table.concat(argv, "|") or "none")
          end,
        })
        "#,
    )]);
    let host = h.host();
    let probe = |toml: &str| {
        host.dispatch(
            HookEvent::TurnStart,
            dispatch("s1", serde_json::json!({ "toml": toml })),
        )
        .context
        .join("")
    };
    assert_eq!(
        probe("model = \"o3\"\nnotify = [\"say\", \"done\"]\n"),
        "say|done"
    );
    assert_eq!(probe("notify = [\"notify-send\"]"), "notify-send");
    assert_eq!(probe("model = \"o3\""), "none", "no notify key, no program");
    assert_eq!(
        probe("# notify = [\"evil\"]\nmodel = \"o3\""),
        "none",
        "a commented-out notify is not a notify"
    );
}

#[test]
fn exec_runs_a_command_feeds_it_stdin_and_kills_one_that_overruns() {
    let h = Hooks::new(&[(
        "exec.lua",
        r#"
        bough.api.create_autocmd("TurnStart", {
          callback = function()
            local r = bough.exec("cat; echo ' :' $?", { stdin = "fed in" })
            bough.context(r.stdout)
            local _, err = bough.exec("sleep 30", { timeout_ms = 300 })
            bough.context(err and "killed" or "not killed")
          end,
        })
        "#,
    )]);
    let out = h
        .host()
        .dispatch(HookEvent::TurnStart, dispatch("s1", serde_json::json!({})));
    assert!(out.context[0].starts_with("fed in"), "{:?}", out.context);
    assert_eq!(out.context[1], "killed");
}
