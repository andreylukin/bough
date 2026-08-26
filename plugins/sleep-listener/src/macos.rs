//! Invariant: the IOKit registration lives on ITS OWN THREAD with its own `CFRunLoop`, and the
//! sleep acknowledgement (`IOAllowPowerChange`) is sent IMMEDIATELY on
//! `kIOMessageSystemWillSleep` — before any bough work — because the system waits on it and a slow
//! acknowledgement is a visible stall for Andrey.
//!
//! Teardown stops the run loop and joins the thread; nothing is left registered.

use std::sync::Arc;

use bough_plugin_power::{PowerEvent, PowerSource};

/// The IOKit-backed source.
pub struct IokitSource {
    last: parking_lot::Mutex<Option<PowerEvent>>,
}

impl IokitSource {
    /// Register with IOKit and start the run loop on a dedicated thread. `Err` when
    /// `IORegisterForSystemPower` returns a null port — the caller then falls back to NSWorkspace.
    /// WP-8.
    pub fn start(
        on_event: Arc<dyn Fn(PowerEvent) + Send + Sync>,
    ) -> Result<Arc<IokitSource>, String> {
        let _ = on_event;
        todo!("WP-8")
    }

    /// Stop the run loop and join the thread. WP-8.
    pub fn stop(&self) {
        todo!("WP-8")
    }
}

impl PowerSource for IokitSource {
    fn kind(&self) -> &'static str {
        "iokit"
    }
    fn last(&self) -> Option<PowerEvent> {
        self.last.lock().clone()
    }
}

/// The NSWorkspace FALLBACK. Used only when IOKit gives no port; it misses dark wakes.
pub struct NsWorkspaceSource {
    last: parking_lot::Mutex<Option<PowerEvent>>,
}

impl NsWorkspaceSource {
    /// Subscribe to `NSWorkspaceDidWakeNotification` / `WillSleepNotification`. WP-8.
    pub fn start(
        on_event: Arc<dyn Fn(PowerEvent) + Send + Sync>,
    ) -> Result<Arc<NsWorkspaceSource>, String> {
        let _ = on_event;
        todo!("WP-8")
    }
}

impl PowerSource for NsWorkspaceSource {
    fn kind(&self) -> &'static str {
        "nsworkspace"
    }
    fn last(&self) -> Option<PowerEvent> {
        self.last.lock().clone()
    }
}
