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

    let _ = std::fs::remove_dir_all(&home);
}
