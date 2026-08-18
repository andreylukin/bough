//! A bough plugin is ONE DIRECTORY contributing hooks, skills and extensions.
//!
//! An integration test rather than a unit one, because the claim is that all
//! three surfaces agree on where plugins live — which is only observable with
//! `BOUGH_HOME` set for a whole process, not mutated inside a shared one.

use std::path::Path;

#[test]
fn a_bough_plugin_directory_contributes_all_three_surfaces() {
    let home = std::env::temp_dir().join(format!("bough-plugin-e2e-{}", uuid::Uuid::new_v4()));
    let plugin = home.join("plugins").join("acme");
    std::fs::create_dir_all(plugin.join("hooks")).unwrap();
    std::fs::create_dir_all(plugin.join("skills").join("review")).unwrap();
    std::fs::create_dir_all(plugin.join("extensions")).unwrap();
    std::fs::write(
        plugin.join("hooks/guard.lua"),
        "bough.api.create_autocmd('TurnStart', { callback = function() end })",
    )
    .unwrap();
    std::fs::write(
        plugin.join("skills/review/SKILL.md"),
        "---\ndescription: d\n---\nbody",
    )
    .unwrap();
    std::fs::write(
        plugin.join("extensions/gh.js"),
        "module.exports = { pr: () => 1 };",
    )
    .unwrap();

    // SAFETY: an integration test is its own process, and this runs before
    // anything else in it reads the environment.
    unsafe { std::env::set_var("BOUGH_HOME", &home) };
    let ws = Path::new("/tmp");

    // The hook is addressable by the PLUGIN's name, which is the whole reason
    // a plugin is a directory rather than three loose files.
    let hooks: Vec<String> = bough_core::hooks::sources::all_sources()
        .iter()
        .flat_map(bough_core::hooks::sources::files_in)
        .map(|(id, _)| id)
        .collect();
    assert!(hooks.contains(&"acme/guard.lua".to_string()), "{hooks:?}");

    let skills = bough_core::skills::list_skills(&bough_core::skills::sources_for(ws));
    let review = skills
        .iter()
        .find(|s| s.name == "review")
        .unwrap_or_else(|| panic!("the plugin's skill is listed: {skills:?}"));
    assert_eq!(review.source, bough_core::skills::SkillSourceName::Plugin);

    let ext = bough_core::extensions::for_workspace(ws);
    assert!(
        ext.fns.iter().any(|f| f.name == "pr"),
        "{:?} {:?}",
        ext.fns,
        ext.errors
    );

    // ---- and the switchboard reaches all three ------------------------------
    //
    // The other half of "a plugin is one directory": a unit you install in one
    // move must come apart, or the only way to stop one piece of it is to
    // delete a file the plugin puts back on its next update.
    bough_core::plugins::set_enabled("acme/skills/review", false).unwrap();
    bough_core::plugins::set_enabled("acme/extensions/gh.js", false).unwrap();
    let skills = bough_core::skills::list_skills(&bough_core::skills::sources_for(ws));
    assert!(
        !skills.iter().any(|s| s.name == "review"),
        "a switched-off skill is not listed: {skills:?}"
    );
    assert!(
        bough_core::extensions::for_workspace(ws).fns.is_empty(),
        "nor is a switched-off extension bound"
    );

    // The plugin's own switch takes the lot, hooks included — and a hook that
    // is off is off because its SOURCE is gone, not because it was skipped.
    bough_core::plugins::set_enabled("acme/guard.lua", true).unwrap();
    bough_core::plugins::set_enabled("acme", false).unwrap();
    let hooks: Vec<String> = bough_core::hooks::sources::all_sources()
        .iter()
        .flat_map(bough_core::hooks::sources::files_in)
        .map(|(id, _)| id)
        .collect();
    assert!(!hooks.contains(&"acme/guard.lua".to_string()), "{hooks:?}");

    // Turning it back on restores the picture you left rather than a blank
    // one: the hook you had enabled is enabled, the two you switched off are
    // still off.
    bough_core::plugins::set_enabled("acme", true).unwrap();
    let items = bough_core::plugins::list();
    let acme = items.iter().find(|p| p.name == "acme").expect("listed");
    let on = |id: &str| -> bool {
        acme.items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("{id} is listed: {acme:?}"))
            .enabled
    };
    assert!(on("acme/guard.lua"));
    assert!(!on("acme/skills/review"));
    assert!(!on("acme/extensions/gh.js"));

    let _ = std::fs::remove_dir_all(&home);
}
