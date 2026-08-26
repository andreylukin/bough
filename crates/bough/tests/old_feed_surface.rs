//! §17 Phase 3: "`command_history` is NOT mail: it stays competence memory, QUERIED FOR PRIMING,
//! never delivered." The negative half has always held (nothing delivers it, and the row's own
//! invariant enforces that). This file is the POSITIVE half: the priming query has a runtime path
//! a human can reach. `OldFeedHandle::prime` and `::notes` used to be called from the adapter's
//! own tests and from nowhere else — a capability with one role, which §0.2 says is not a seam.
//!
//! And the hermeticity claim underneath it: the shipped `old-feed` defaults resolve against
//! `$HOME`, so a test that boots the shipped bundle must point `$HOME` at its own scratch dir or
//! it reads the developer's real databases.

mod support;

use bough_plugin_commands::{CommandCx, Commands};
use bough_plugin_hello::trace;
use bough_plugin_old_feed_adapter::OldFeed;
use support::{boot_real, row_ctx, TempDir};

async fn boot_tui() -> (std::sync::Arc<bough_kernel::Kernel>, TempDir) {
    boot_real(
        "tui",
        &[
            support::fixture("llm-replay.yml"),
            // §17 Phase 6 disabled the shipped row; this file tests the adapter, so it turns
            // exactly that row back on — which is also the revert path working.
            support::fixture("old-feed-on.yml"),
        ],
    )
    .await
}

/// `/prime` is registered by the shipped tree and dispatching it runs the priming query — no
/// model turn, no step, and the output says in as many words that this is not mail.
#[tokio::test]
async fn the_priming_query_is_reachable_as_a_human_command() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_tui().await;

    let commands = kernel
        .root()
        .peek_live::<Commands>()
        .expect("`commands` is bound");
    let names: Vec<String> = commands
        .list(None)
        .into_iter()
        .map(|c| c.name.to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "prime"),
        "§14's priming half must have a surface: {names:?}"
    );

    let ctx = row_ctx(&kernel, "old-feed");
    let inv = bough_plugin_commands::parse("/prime", '/').expect("a command line");
    let out = commands
        .dispatch(
            inv,
            CommandCx {
                ctx,
                agent: None,
                at: chrono::Utc::now(),
            },
        )
        .await
        .expect("`/prime` dispatches");
    assert!(
        out.text.contains("never delivered") || out.text.contains("no command memory"),
        "the output must say what command memory is: {}",
        out.text
    );

    kernel.shutdown().await;
}

/// The hermeticity half: booting the shipped bundle in a test must not open the developer's real
/// `~/.bough/bough.db` or `~/.jungler/jungler.db`. Both defaults are `!!expr home_path(..)`, so
/// the proof is that the row reports them ABSENT under the scratch home.
#[tokio::test]
async fn the_shipped_old_feed_defaults_resolve_under_the_test_home() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_tui().await;

    let feed = kernel
        .root()
        .peek_live::<OldFeed>()
        .expect("`old_feed` is bound");
    let disabled = feed.status().disabled;
    let scratch = dir.path().to_string_lossy().to_string();
    assert!(
        !disabled.is_empty(),
        "a scratch home has neither old database, so both sources report disabled"
    );
    for (source, why) in &disabled {
        assert!(
            why.contains(&scratch),
            "`{source}` must name a path under the test home, not the developer's: {why}"
        );
    }

    kernel.shutdown().await;
}
