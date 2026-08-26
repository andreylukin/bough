//! §9: `restrict` is VISIBILITY COMPOSITION, not an authority boundary — and a filtered-away tool
//! is indistinguishable from one that never existed, message included.

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{FailureClass, Restrict, ToolName, ToolsError};
use parking_lot::Mutex;
use support::{agent, call, ctx, registry_with, spec, Stub};

fn stub() -> Arc<dyn bough_plugin_tools::Tool> {
    Arc::new(Stub {
        safe: true,
        delay: Duration::from_millis(0),
        log: Arc::new(Mutex::new(Vec::new())),
    })
}

fn names(set: &[&str]) -> BTreeSet<ToolName> {
    set.iter().map(ToolName::new).collect()
}

#[tokio::test]
async fn a_restricted_tool_is_absent_from_the_schema() {
    let ctx = ctx();
    let tools = registry_with(&ctx, vec![spec("bash", stub()), spec("read_file", stub())]).await;
    assert_eq!(tools.schemas(&agent()).len(), 2);

    tools
        .restrict(
            &ctx,
            &agent(),
            Restrict {
                allow: None,
                deny: names(&["bash"]),
            },
        )
        .await
        .unwrap();

    let shown: Vec<String> = tools
        .schemas(&agent())
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(shown, vec!["read_file".to_string()]);
    assert_eq!(tools.visible(&agent()), vec![ToolName::new("read_file")]);

    // Another agent is untouched: a restriction is per-agent.
    let other = AgentName::new("other");
    assert_eq!(tools.schemas(&other).len(), 2);
}

#[tokio::test]
async fn a_restricted_tool_is_refused_indistinguishably_from_a_nonexistent_one() {
    let ctx = ctx();
    let tools = registry_with(&ctx, vec![spec("bash", stub())]).await;
    tools
        .restrict(
            &ctx,
            &agent(),
            Restrict {
                allow: None,
                deny: names(&["bash"]),
            },
        )
        .await
        .unwrap();

    let restricted = tools
        .resolve(&agent(), &ToolName::new("bash"))
        .err()
        .expect("a restricted tool is refused");
    let absent = tools
        .resolve(&agent(), &ToolName::new("never_existed"))
        .err()
        .expect("a nonexistent tool is refused");
    assert!(matches!(restricted, ToolsError::NotFound { .. }));
    assert!(matches!(absent, ToolsError::NotFound { .. }));
    assert_eq!(
        restricted.to_string().replace("bash", "X"),
        absent.to_string().replace("never_existed", "X"),
        "the two messages differ only in the name that was asked for"
    );

    // ...and the same holds through the EXECUTOR, which is where the model actually meets it.
    let out = tools
        .execute(&ctx, vec![call("bash", "a"), call("never_existed", "b")])
        .await;
    assert!(!out[0].ok && !out[1].ok);
    let restricted = out[0].failure.as_ref().unwrap();
    let absent = out[1].failure.as_ref().unwrap();
    assert_eq!(restricted.kind, FailureClass::NotFound);
    assert_eq!(absent.kind, FailureClass::NotFound);
    assert_eq!(
        restricted.message.replace("bash", "X"),
        absent.message.replace("never_existed", "X"),
        "the executor's two refusals differ only in the name asked for"
    );
    assert_eq!(out[0].value, None);
    assert!(
        !restricted.message.contains("restrict") && !restricted.message.contains("denied"),
        "a filtered tool must not leak that it exists: {}",
        restricted.message
    );
}

#[tokio::test]
async fn two_restrictions_compose_as_an_intersection() {
    let ctx = ctx();
    let tools = registry_with(
        &ctx,
        vec![
            spec("bash", stub()),
            spec("grep", stub()),
            spec("glob", stub()),
        ],
    )
    .await;
    tools
        .restrict(
            &ctx,
            &agent(),
            Restrict {
                allow: Some(names(&["bash", "grep"])),
                deny: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
    tools
        .restrict(
            &ctx,
            &agent(),
            Restrict {
                allow: Some(names(&["grep", "glob"])),
                deny: BTreeSet::new(),
            },
        )
        .await
        .unwrap();

    assert_eq!(
        tools.visible(&agent()),
        vec![ToolName::new("grep")],
        "a second restriction can only NARROW"
    );
}

#[tokio::test]
async fn disposing_a_restriction_restores_visibility() {
    let ctx = ctx();
    let tools = registry_with(&ctx, vec![spec("bash", stub())]).await;
    let handle = tools
        .restrict(
            &ctx,
            &agent(),
            Restrict {
                allow: None,
                deny: names(&["bash"]),
            },
        )
        .await
        .unwrap();
    assert!(tools.visible(&agent()).is_empty());
    handle.dispose().await;
    assert_eq!(tools.visible(&agent()), vec![ToolName::new("bash")]);
}
