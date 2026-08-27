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
    kernel.shutdown().await;
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
/// terminal, print `bough: shutdown timed out after {ms}ms; leaving anyway` to stderr, and exit
/// with `code`. The deadline is [`crate::cli::Cli::shutdown_ms`], never a constant at the call
/// site — a hang with the alt screen still up is the worst exit the product has.
pub async fn shutdown_bounded(kernel: &bough_kernel::Kernel, ms: u64, code: u8) -> ExitOutcome {
    let _ = (kernel, ms, code);
    todo!("WP-1")
}

/// How teardown ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitOutcome {
    Clean,
    TimedOut,
}
