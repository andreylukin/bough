//! P3-D9: the `RenderIntent::Diff` args convention, checked against the REAL argument names of the
//! two `tools-baseline` tools that declare `Diff`, so the convention and the tools cannot drift.

use crate::common;

use bough_plugin_tools::{RenderIntent, ToolScope};
use bough_plugin_tools_baseline::BaselineConfig;
use bough_plugin_tui_render::{diff_spec_from_args, DiffSpec};
use std::sync::Arc;

fn baseline_specs() -> Vec<bough_plugin_tools::ToolSpec> {
    bough_plugin_tools_baseline::specs(Arc::new(BaselineConfig {
        bash_tags_min: 3,
        bash_tags_max: 5,
        root: std::path::PathBuf::from("/tmp"),
        bash_timeout_ms: 1000,
        max_output_bytes: 1024,
        max_read_bytes: 1024,
        deny_globs: Vec::new(),
    }))
}

/// The property names a spec's input schema declares.
fn props(spec: &bough_plugin_tools::ToolSpec) -> Vec<String> {
    let v = serde_json::to_value(&spec.input_schema).expect("a schema is json");
    v.get("properties")
        .and_then(|p| p.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn every_diff_tool_in_the_baseline_satisfies_the_convention() {
    let specs = baseline_specs();
    let diff_tools: Vec<_> = specs
        .iter()
        .filter(|s| s.render == RenderIntent::Diff)
        .collect();
    assert!(!diff_tools.is_empty(), "the baseline declares Diff tools");
    for s in diff_tools {
        assert!(matches!(s.scope, ToolScope::Global));
        let p = props(s);
        let args: serde_json::Value = p
            .iter()
            .map(|k| (k.clone(), serde_json::Value::String(format!("<{k}>"))))
            .collect::<serde_json::Map<_, _>>()
            .into();
        assert!(
            diff_spec_from_args(&args).is_some(),
            "{} declares Diff but its args {p:?} match no documented shape",
            s.name.as_str()
        );
    }
}

#[test]
fn edit_file_uses_the_old_new_shape_and_write_file_the_content_shape() {
    let specs = baseline_specs();
    let edit = specs
        .iter()
        .find(|s| s.name.as_str() == "edit_file")
        .expect("edit_file");
    let write = specs
        .iter()
        .find(|s| s.name.as_str() == "write_file")
        .expect("write_file");
    let mut e = props(edit);
    e.sort();
    assert_eq!(e, vec!["new", "old", "path"]);
    let mut w = props(write);
    w.sort();
    assert_eq!(w, vec!["content", "path"]);
}

#[test]
fn the_three_documented_shapes_are_accepted() {
    assert_eq!(
        diff_spec_from_args(&serde_json::json!({"path": "a.rs", "old": "x", "new": "y"})),
        Some(DiffSpec {
            path: Some("a.rs".into()),
            before: "x".into(),
            after: "y".into()
        })
    );
    assert_eq!(
        diff_spec_from_args(
            &serde_json::json!({"path": "a.rs", "old_string": "x", "new_string": "y"})
        ),
        Some(DiffSpec {
            path: Some("a.rs".into()),
            before: "x".into(),
            after: "y".into()
        })
    );
    assert_eq!(
        diff_spec_from_args(&serde_json::json!({"path": "a.rs", "content": "y"})),
        Some(DiffSpec {
            path: Some("a.rs".into()),
            before: String::new(),
            after: "y".into()
        })
    );
}

#[test]
fn a_fourth_shape_is_rejected() {
    assert_eq!(
        diff_spec_from_args(&serde_json::json!({"file": "a.rs", "patch": "@@ -1 +1 @@"})),
        None
    );
    assert_eq!(diff_spec_from_args(&serde_json::json!("a string")), None);
}

#[test]
fn the_shapes_are_tried_in_the_documented_order() {
    // A tool that carries BOTH `old`/`new` and `content` renders the edit, not the whole file.
    let spec = diff_spec_from_args(
        &serde_json::json!({"path": "a.rs", "old": "x", "new": "y", "content": "z"}),
    )
    .expect("matched");
    assert_eq!(spec.before, "x");
    assert_eq!(spec.after, "y");
}

#[test]
fn an_expanded_diff_body_renders_as_recorded() {
    let th = common::theme();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "old": "fn a() {}\nfn b() {}\nfn c() {}\n",
        "new": "fn a() {}\nfn b2() {}\nfn c() {}\nfn d() {}\n",
    });
    let view = bough_plugin_tui_render::ToolCallView {
        name: "edit_file",
        intent: RenderIntent::Diff,
        args: &args,
        result: None,
        expanded: true,
        width: 40,
        theme: &th,
    };
    let body: Vec<String> = bough_plugin_tui_render::tool_body(&view, 50)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    insta::assert_snapshot!(body.join("\n"));
}
