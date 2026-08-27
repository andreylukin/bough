//! Invariant: teardown before exit (§0.1 item 2). Every exit path — a failed activation assertion,
//! SIGINT, a `--check` run — awaits `kernel.shutdown()` before returning, so a Phase-3 TUI failure
//! still restores the terminal.
//!
//! And: an enabled row that never activates is a BOOT FAILURE (§0.2, Decision D12). At boot it is
//! fatal and names every unresolved row with its unmet keys; during a live recompose it is a
//! `kernel/rows-unresolved` warning and the tree stays.

use std::process::ExitCode;
use std::sync::Arc;

use bough_kernel::{Catalog, Kernel, KernelOptions, TreeSnapshot};

use crate::cli::{BootError, Cli};
use crate::compose::compose_plan;

/// Compose, mount, quiesce, assert, then either run or exit.
pub async fn boot(mut cli: Cli) -> Result<ExitCode, BootError> {
    // A subcommand SELECTS a composition; it never branches the boot path (§0.1 item 2).
    crate::exec::force_profile(&mut cli);
    // The SIGINT handler is installed BEFORE anything is composed. `tokio::signal::ctrl_c()`
    // registers on its first poll, so awaiting it only after the tree is mounted leaves a window
    // — the whole of boot — in which SIGINT hits the default handler and the process dies without
    // tearing down. Arming it here closes that window: a signal during boot is remembered and
    // acted on the moment the tree is up.
    let interrupted = Arc::new(tokio::sync::Notify::new());
    {
        let armed = Arc::clone(&interrupted);
        tokio::spawn(async move {
            if let Err(e) = tokio::signal::ctrl_c().await {
                eprintln!("bough: could not listen for SIGINT: {e}");
            }
            // Wake a waiter if boot has already reached the select, and leave a permit if it has
            // not: a signal that arrives mid-boot must not be dropped.
            armed.notify_waiters();
            armed.notify_one();
        });
    }
    let cli = Arc::new(cli);
    let catalog = Catalog::from_inventory()?;
    let (profile, composition) = compose_plan(&cli, &catalog)?;

    report_warnings(&composition.warnings);

    // `--dump-config` prints `render()` of exactly this `Composition` and mounts nothing (V6).
    if cli.dump_config {
        print!(
            "{}",
            bough_kernel::render(&composition, cli.dump_format.into())
        );
        return Ok(ExitCode::SUCCESS);
    }

    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: profile.name.clone(),
            invariants: profile.invariants,
        },
    );

    kernel.load(composition).await?;
    let quiesced = kernel.quiesce().await;
    if !quiesced {
        eprintln!(
            "bough: the tree did not reach a quiescent state; treating that as a boot failure"
        );
    }

    let snapshot = kernel.snapshot();
    if !quiesced || assert_all_activated(&snapshot).is_err() {
        // Teardown FIRST, then the report (§0.1 item 2, P3-D3's neighbour). A Phase-3 surface row
        // owns the alt screen, so a report printed before `shutdown()` is written INTO the alt
        // screen and then wiped by the restore — the failure would be invisible on the very path
        // that most needs to be readable. Shutting down first leaves the normal screen, raw mode
        // off and the cursor back, and the report lands where Andrey can read it (V8).
        kernel.shutdown().await;
        eprint!("{}", describe_unresolved(&snapshot));
        return Ok(ExitCode::FAILURE);
    }

    if cli.check {
        kernel.shutdown().await;
        return Ok(ExitCode::SUCCESS);
    }

    let watch = if cli.no_watch {
        None
    } else {
        Some(crate::watch::watch_user_patch(
            Arc::clone(&kernel),
            Arc::clone(&cli),
        ))
    };

    // M15's listener is NOT here. It is an effect of `tui-shell`, the row whose surface it
    // drives (§0.1 item 2: the launcher owns no behaviour of its own): a handle captured here at
    // boot goes stale the moment the `tui` row reloads — which a saved patch file, the very event
    // being reported, can cause — and a `tui` row disabled by patch must take its listener with it.

    // The launcher owns composition and teardown, and nothing else. A surface is a ROW, and it
    // keeps the process alive by holding the runtime; the two ways out are SIGINT and a row
    // asking through `Kernel::request_exit` (P2-D23). Both tear down first.
    let code = tokio::select! {
        code = kernel.exited() => code,
        _ = interrupted.notified() => 0,
    };
    if let Some(w) = watch {
        w.stop().await;
    }
    // B8: teardown is BOUNDED. A row that never quiesces used to hang the process with the alt
    // screen still up; now the terminal comes back and the launcher leaves.
    shutdown_bounded(&kernel, cli.shutdown_ms).await;
    // Whichever restore path ran took the farewell with it. If none did — a headless profile,
    // where there was no terminal to restore — the launcher still owes the user the line.
    if let Some(farewell) = bough_plugin_tui_shell::term::take_farewell() {
        println!("{farewell}");
    }
    Ok(ExitCode::from(code))
}

/// Print composition warnings. Shared by boot and by the live watch path, so an absent row id is
/// reported the same way whichever produced it (§0.2, §0.5).
pub fn report_warnings(warnings: &[bough_kernel::ComposeWarning]) {
    for w in warnings {
        match w {
            bough_kernel::ComposeWarning::AbsentRowId { layer, id } => {
                eprintln!("bough: layer `{layer}` names row `{id}`, which no layer created");
            }
        }
    }
}

/// After quiesce, every row with `disabled == false` must be ACTIVE.
///
/// On failure the caller prints each unresolved row with its unmet keys, awaits
/// `kernel.shutdown()`, and exits 1.
pub fn assert_all_activated(s: &TreeSnapshot) -> Result<(), BootError> {
    let unresolved = s.unresolved();
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(BootError::Unresolved(unresolved.len()))
    }
}

/// Render the unresolved rows for the boot-failure message: one line per row, naming the row, its
/// plugin and each unmet key.
pub fn describe_unresolved(s: &TreeSnapshot) -> String {
    let rows = s.unresolved();
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    out.push_str(&format!(
        "bough: {} enabled row(s) never activated:\n",
        rows.len()
    ));
    for r in &rows {
        let plugin = r.plugin.as_deref().unwrap_or("<no plugin>");
        let unmet = if r.unmet.is_empty() {
            "-".to_string()
        } else {
            r.unmet.join(", ")
        };
        out.push_str(&format!(
            "  {} (plugin `{}`) is {:?}; unmet: {}\n",
            r.id, plugin, r.state, unmet
        ));
        if let Some(e) = &r.error {
            out.push_str(&format!("      error: {e}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::config::Fingerprint;
    use bough_kernel::{EntryId, FiberState, RowSnapshot};
    use std::collections::BTreeMap;

    fn row(id: &str, state: FiberState, disabled: bool, unmet: &[&str]) -> RowSnapshot {
        RowSnapshot {
            id: EntryId::new(id),
            plugin: Some(format!("{id}-plugin")),
            uid: None,
            state,
            disabled,
            unmet: unmet.iter().map(|s| s.to_string()).collect(),
            error: None,
            provides: Vec::new(),
            realms: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    fn snapshot(rows: Vec<RowSnapshot>) -> TreeSnapshot {
        TreeSnapshot {
            fingerprint: Fingerprint::of(&[]),
            rows,
        }
    }

    #[test]
    fn assert_all_activated_passes_on_a_full_tree() {
        let s = snapshot(vec![
            row("greeting.provider", FiberState::Active, false, &[]),
            row("hello.greeter", FiberState::Active, false, &[]),
        ]);
        assert!(assert_all_activated(&s).is_ok());
        assert_eq!(describe_unresolved(&s), "");
    }

    #[test]
    fn assert_all_activated_names_every_unresolved_row_and_its_unmet_keys() {
        let s = snapshot(vec![
            row("greeting.provider", FiberState::Active, false, &[]),
            row("hello.greeter", FiberState::Pending, false, &["greeting"]),
            row("other.row", FiberState::Pending, false, &["a", "b"]),
        ]);
        let err = assert_all_activated(&s).unwrap_err();
        assert!(matches!(err, BootError::Unresolved(2)), "{err}");

        let msg = describe_unresolved(&s);
        assert!(msg.contains("hello.greeter"), "{msg}");
        assert!(msg.contains("greeting"), "{msg}");
        assert!(msg.contains("other.row"), "{msg}");
        assert!(msg.contains("a, b"), "{msg}");
        assert!(
            !msg.contains("greeting.provider"),
            "an ACTIVE row must not be reported: {msg}"
        );
    }

    #[test]
    fn disabled_rows_are_not_required_to_activate() {
        let s = snapshot(vec![
            row("greeting.provider", FiberState::Active, false, &[]),
            row("hello.greeter", FiberState::Inactive, true, &["greeting"]),
        ]);
        assert!(
            assert_all_activated(&s).is_ok(),
            "a disabled row is not a boot failure"
        );
    }
}

/// Await `kernel.shutdown()` under a deadline (phase ux1 §2.4, B8). On timeout: restore the
/// terminal and print `bough: shutdown timed out after {ms}ms; leaving anyway` to stderr. It does
/// NOT exit — the caller owns the process's exit code, and this used to take a `code` it ignored.
/// The deadline is [`crate::cli::Cli::shutdown_ms`], never a constant at the call site: a hang
/// with the alt screen still up is the worst exit the product has.
pub async fn shutdown_bounded(kernel: &bough_kernel::Kernel, ms: u64) -> ExitOutcome {
    bounded(kernel.shutdown(), ms).await
}

/// The deadline, over ANY teardown future. Split out so the timeout path is testable against a
/// fiber that genuinely never quiesces — which is not something a `Kernel` can be asked to be.
pub async fn bounded<F: std::future::Future<Output = ()>>(shutdown: F, ms: u64) -> ExitOutcome {
    match tokio::time::timeout(std::time::Duration::from_millis(ms), shutdown).await {
        Ok(()) => ExitOutcome::Clean,
        Err(_) => {
            // The terminal comes back BEFORE the message: a report printed into a live alt screen
            // is a report nobody reads (the same lesson `boot` already carries above).
            bough_plugin_tui_shell::restore_now();
            eprintln!("bough: shutdown timed out after {ms}ms; leaving anyway");
            ExitOutcome::TimedOut
        }
    }
}

/// How teardown ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitOutcome {
    Clean,
    TimedOut,
}

#[cfg(test)]
mod bounded_teardown_tests {
    use super::*;

    /// A teardown that finishes inside the deadline is CLEAN, and restores nothing itself.
    #[tokio::test]
    async fn a_teardown_that_finishes_is_clean() {
        assert_eq!(
            bounded(std::future::ready(()), 2000).await,
            ExitOutcome::Clean
        );
    }

    /// B8, the case that hung the product: a fiber whose teardown never returns. The deadline
    /// expires, the terminal is restored, and the launcher LEAVES. `arm_for_test` makes the
    /// restore observable without a terminal to enter.
    #[tokio::test]
    async fn a_teardown_that_never_finishes_times_out_and_still_restores_the_terminal() {
        bough_plugin_tui_shell::term::arm_for_test();
        let before = bough_plugin_tui_shell::term::restores();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            bounded(std::future::pending::<()>(), 50),
        )
        .await
        .expect("the deadline is the point: this must not be what times out");

        assert_eq!(outcome, ExitOutcome::TimedOut);
        assert_eq!(
            bough_plugin_tui_shell::term::restores(),
            before + 1,
            "the terminal came back before the launcher left"
        );
        assert!(
            !bough_plugin_tui_shell::term::is_entered(),
            "and nothing is left entered"
        );
    }
}
