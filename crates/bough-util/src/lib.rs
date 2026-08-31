//! Invariant: `bough-util` is pure. It creates no tokio runtime, installs no logger, caches nothing
//! in a global, and knows no domain vocabulary. Everything here is a leaf helper that a plugin, the
//! kernel or the launcher may call without acquiring a dependency on any of them (§0.1 item 3).

pub mod home;
pub mod id;
pub mod time;

pub use home::{bough_home, bough_path, ensure_dir, home_dir, ui_patch_path, user_patch_path};
pub use time::{with_timeout, Deadline, TimedOut};
