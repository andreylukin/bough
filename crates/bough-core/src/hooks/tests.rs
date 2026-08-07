//! The hook contract, driven through a real Luau interpreter over real files
//! in a temp directory — the same path production takes, minus `~/.bough`.

use super::*;

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
        pattern: pattern.into(),
        data,
    }
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
