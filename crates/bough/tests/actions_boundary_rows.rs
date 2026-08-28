//! The two outward-facing Provider rows of the SHIPPED tree, on a machine configured the way
//! `bundles/bough-base.yml` ships them.
//!
//! `actions.linear`'s key defaults to `env_or("LINEAR_API_KEY", "")`. With no key, the row must
//! still ACTIVATE (a machine without a Linear key boots — §0.2) and must register NOTHING: an
//! advertised `linear_write` that fails at the endpoint with an opaque HTTP 401, inside an
//! idempotency journal row, is the silent-misconfiguration shape §0.2 refuses. The refusal
//! belongs at the executor, as `NoProvider`.

use crate::support;

use bough_kernel::FiberState;
use bough_plugin_actions::{ActionKind, Actions};
use bough_plugin_hello::trace;
use support::{boot_real, row, row_ctx};

#[tokio::test]
async fn with_no_linear_key_the_row_activates_and_linear_write_is_not_a_registered_kind() {
    let _guard = trace::test_lock();
    // `boot_real` isolates `$HOME` and `$BOUGH_HOME`, but not the environment the expression
    // reads; the gate must behave the same on a developer machine that HAS a key.
    // SAFETY: the fixture holds the process-wide test lock.
    unsafe { std::env::remove_var("LINEAR_API_KEY") };
    let (kernel, _dir) = boot_real("headless", &[]).await;

    assert_eq!(
        row(&kernel, "actions.linear").state,
        FiberState::Active,
        "a machine with no Linear key still boots"
    );
    let actions = row_ctx(&kernel, "actions.linear")
        .get::<Actions>()
        .expect("`actions` is bound");
    let kinds = actions.kinds();
    assert!(
        !kinds.contains(&ActionKind::LinearWrite),
        "an unkeyed row advertises nothing: {kinds:?}"
    );
    // …while GitHub's three, which need no key of their own, are there.
    for k in [
        ActionKind::OpenPr,
        ActionKind::PushToPr,
        ActionKind::BotThreadOp,
    ] {
        assert!(kinds.contains(&k), "{k:?} is missing from {kinds:?}");
    }
}
