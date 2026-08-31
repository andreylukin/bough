//! The macOS FFI, exercised for real on macOS: the IOKit registration goes up on its own thread
//! and comes back down cleanly, and the NSWorkspace fallback's observer receives a notification
//! posted into the workspace's own notification center — the one thing about that path that can be
//! checked without closing a lid.
//!
//! On every other platform this file is empty and `noop.rs` is what the row uses; the platform rule
//! itself is `choose`'s unit tests, which run everywhere.

#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::time::Duration;

use bough_plugin_power::PowerEvent;
use bough_plugin_sleep_listener::macos::{IokitSource, NsWorkspaceSource};
use bough_plugin_sleep_listener::Gate;
use parking_lot::Mutex;

fn gate(min_ms: u64) -> (Arc<Gate>, Arc<Mutex<Vec<PowerEvent>>>) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&out);
    (
        Gate::new(
            min_ms,
            Arc::new(move |ev| sink.lock().push(ev)) as Arc<dyn Fn(PowerEvent) + Send + Sync>,
        ),
        out,
    )
}

#[test]
fn the_iokit_registration_goes_up_on_its_own_thread_and_comes_back_down() {
    let (g, out) = gate(1);
    let source = IokitSource::start(Arc::clone(&g)).expect("IORegisterForSystemPower gives a port");
    // Nothing has happened to the machine, so nothing was dispatched.
    assert!(out.lock().is_empty());
    assert_eq!(g.last(), None);

    // Teardown stops the run loop and joins the thread; a leak or a deadlock hangs here.
    source.stop();
    // Idempotent: `Drop` runs `stop` again when the Arc goes.
    source.stop();
    drop(source);
    assert!(out.lock().is_empty(), "teardown dispatches nothing");
}

#[test]
fn two_registrations_can_live_at_once_and_tear_down_independently() {
    let (a, _oa) = gate(1);
    let (b, _ob) = gate(1);
    let one = IokitSource::start(a).expect("first registration");
    let two = IokitSource::start(b).expect("second registration");
    drop(one);
    drop(two);
}

#[test]
fn the_nsworkspace_observer_receives_a_posted_sleep_and_wake() {
    let (g, out) = gate(1);
    let source = NsWorkspaceSource::start(Arc::clone(&g)).expect("the observer registers");

    NsWorkspaceSource::post("NSWorkspaceWillSleepNotification");
    // The gate's floor is 1ms, and a wake in the same millisecond as its sleep is correctly
    // dropped; this is the test waiting out its own floor, not a timing hope.
    std::thread::sleep(Duration::from_millis(5));
    NsWorkspaceSource::post("NSWorkspaceDidWakeNotification");

    let seen = out.lock().clone();
    assert_eq!(
        seen.len(),
        2,
        "the observer saw both notifications: {seen:?}"
    );
    assert!(matches!(seen[0], PowerEvent::WillSleep { .. }));
    match seen[1] {
        PowerEvent::DidWake { asleep_for, .. } => assert!(
            asleep_for.is_some_and(|d| d < Duration::from_secs(5)),
            "the wake is measured from the sleep this test posted"
        ),
        _ => panic!("expected a wake"),
    }

    // Removing the observer means a later post reaches nobody.
    drop(source);
    NsWorkspaceSource::post("NSWorkspaceDidWakeNotification");
    assert_eq!(out.lock().len(), 2, "a removed observer hears nothing");
}

/// The unload-while-loading race, driven directly. `IokitSource::start` returns as soon as the
/// thread has published its run-loop pointer, which is BEFORE the thread reaches `CFRunLoopRun`;
/// a `CFRunLoopStop` issued in that window is a documented no-op on a run loop whose
/// `_currentMode` is NULL. Before the retry loop in `stop`, the stop was simply lost — the thread
/// then blocked in `CFRunLoopRun` forever and `stop` blocked forever in `join`, holding the
/// `thread` mutex so `Drop` blocked behind it too.
///
/// Twenty start-then-immediately-stop cycles is enough to land inside the window on this machine;
/// if the race is back, this hangs rather than failing, which is exactly what it did in
/// production.
#[test]
fn starting_and_immediately_stopping_never_hangs() {
    for _ in 0..20 {
        let (g, out) = gate(1);
        let source = IokitSource::start(Arc::clone(&g)).expect("a port");
        source.stop();
        assert!(out.lock().is_empty());
    }
}
