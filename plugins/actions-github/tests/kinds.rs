//! §7 — the set of outward acts is CLOSED and it is exactly four. Before a Provider mounts there
//! are none; after both mount there are four; there is never a fifth, and the two acts §7 forbids
//! cannot be spelled at all.

mod support;

use std::sync::Arc;

use bough_plugin_actions::{ActionError, ActionKind, ActionProvider, ActionsHandle};
use bough_plugin_actions_linear::{LinearActionError, LinearActions, LinearApi};
use support::*;

/// A Linear stub that answers nothing: these cases never get as far as a call.
struct NoLinear;

#[async_trait::async_trait]
impl LinearApi for NoLinear {
    async fn graphql(
        &self,
        _q: &str,
        _v: serde_json::Value,
    ) -> Result<serde_json::Value, LinearActionError> {
        Err(LinearActionError::Server("the stub answers nothing".into()))
    }
}

#[tokio::test]
async fn kinds_is_empty_before_a_provider_mounts() {
    let (_ctx, _ledger, actions) = fixture().await;
    assert!(
        actions.kinds().is_empty(),
        "the seam exists before the capability does"
    );
}

#[tokio::test]
async fn after_both_providers_mount_exactly_the_four_kinds_exist() {
    let (ctx, _ledger, actions) = fixture().await;
    let gh = Arc::new(FakeGh::new("andrey"));
    actions
        .provider(&ctx, provider(&gh) as Arc<dyn ActionProvider>)
        .await
        .expect("the github provider registers");
    actions
        .provider(
            &ctx,
            LinearActions::with_api(Arc::new(NoLinear)) as Arc<dyn ActionProvider>,
        )
        .await
        .expect("the linear provider registers");

    let mut kinds = actions.kinds();
    kinds.sort_by_key(|k| k.as_str());
    let mut all = ActionKind::all().to_vec();
    all.sort_by_key(|k| k.as_str());
    assert_eq!(kinds, all, "exactly §7's four, no more and no fewer");
    assert_eq!(kinds.len(), 4);
}

#[tokio::test]
async fn an_unregistered_kind_is_refused_by_the_executor_before_anything_is_journalled() {
    let (ctx, ledger, actions) = fixture().await;
    // Only the GitHub provider mounts, so `linear_write` has no Provider.
    let gh = Arc::new(FakeGh::new("andrey"));
    actions
        .provider(&ctx, provider(&gh) as Arc<dyn ActionProvider>)
        .await
        .expect("the github provider registers");

    let err = actions
        .execute(
            &ctx,
            request(
                ActionKind::LinearWrite,
                "TEAM-1",
                serde_json::json!({ "comment": "hi" }),
            ),
        )
        .await
        .expect_err("no provider claims it");
    match err {
        ActionError::NoProvider(k) => assert_eq!(k, "linear_write", "the refusal NAMES the kind"),
        other => panic!("expected NoProvider, got {other}"),
    }
    let rows = ledger
        .0
        .actions(&Default::default())
        .await
        .expect("the journal reads");
    assert!(
        rows.is_empty(),
        "nothing was attempted, so nothing is journalled"
    );
}

/// `slack_send` and `create_ticket` are not values of [`ActionKind`]: this test is the place a
/// future attempt to add one would have to change, and `ActionKind::all()` is the whole set.
#[test]
fn slack_send_is_not_a_kind_that_can_be_spelled() {
    let spellings: Vec<&str> = ActionKind::all().iter().map(|k| k.as_str()).collect();
    assert_eq!(
        spellings,
        vec!["open_pr", "push_to_pr", "bot_thread_op", "linear_write"]
    );
    assert!(!spellings.contains(&"slack_send"));
    assert!(!spellings.contains(&"create_ticket"));
    // And no JSON spelling of one deserialises into a kind.
    for name in ["slack_send", "create_ticket", "send_email"] {
        assert!(
            serde_json::from_value::<ActionKind>(serde_json::Value::String(name.into())).is_err(),
            "`{name}` must not deserialise into a kind"
        );
    }
}

/// A Provider mounting is an EFFECT: its kinds leave with its registration.
#[tokio::test]
async fn a_providers_kinds_leave_with_its_registration() {
    let (ctx, _ledger, actions) = fixture().await;
    let gh = Arc::new(FakeGh::new("andrey"));
    let handle: bough_kernel::EffectHandle =
        ActionsHandle::provider(&actions, &ctx, provider(&gh) as Arc<dyn ActionProvider>)
            .await
            .expect("the provider registers");
    assert_eq!(actions.kinds().len(), 3);
    handle.dispose().await;
    assert!(actions.kinds().is_empty(), "unload leaves no trace");
}
