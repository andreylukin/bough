//! An installed Claude Code plugin's own hooks reach bough.
//!
//! An integration test rather than a unit one because the only way to observe
//! this is to point `$HOME` at a fixture registry, and `$HOME` is process-wide:
//! mutating it inside the shared test binary poisons every hook test that
//! happens to be running beside it. Its own process is the honest isolation.

use bough_core::hooks::{HookDispatch, HookEvent, HookHost, ToolDecision};

/// `plugin.json`'s `hooks` key is `string | string[] | object` — a POINTER to
/// additional hook files, or an inline block. The adapter read it as though it
/// were always inline, so every plugin that keeps its hooks anywhere but the
/// auto-discovered `hooks/hooks.json` had them silently dropped.
#[test]
fn a_manifest_declared_hooks_file_is_run_with_the_plugin_root_expanded() {
    let home = std::env::temp_dir().join(format!("bough-cc-plugin-{}", uuid::Uuid::new_v4()));
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
                    // `${CLAUDE_PLUGIN_ROOT}` is expanded to the install
                    // directory, the way Claude Code expands it when it spawns.
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

    // SAFETY: an integration test is its own process, and nothing else in it
    // reads the environment concurrently.
    unsafe { std::env::set_var("HOME", &home) };

    let hooks_dir = std::env::temp_dir().join(format!("bough-cc-hooks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        hooks_dir.join("claude-code.lua"),
        include_str!("../hooks/claude-code.lua"),
    )
    .unwrap();

    let ws = std::env::temp_dir().join(format!("bough-cc-ws-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&ws).unwrap();
    let out = HookHost::load(&hooks_dir)
        .expect("the adapter loads")
        .dispatch(
            HookEvent::PreTool,
            HookDispatch {
                session_id: "s1".into(),
                workspace: ws.to_string_lossy().into_owned(),
                pattern: "bash".into(),
                data: serde_json::json!({ "input": { "command": "ls" } }),
            },
        );

    assert_eq!(out.decision, Some(ToolDecision::Deny), "{out:?}");
    assert!(
        out.reason
            .as_deref()
            .unwrap_or_default()
            .contains(&*install.to_string_lossy()),
        "the plugin root reached the command: {out:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&hooks_dir);
    let _ = std::fs::remove_dir_all(&ws);
}
