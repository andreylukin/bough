//! Invariant: a notice carries a ROLE, so the theme can colour an error like an error (M22). A
//! notice is never a bare string at the point it is raised.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// One transient line above the composer.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub kind: NoticeKind,
    pub at: DateTime<Utc>,
    /// `None` waits for the next key (an error); `Some` fades after `notice_ms`.
    pub ttl: Option<Duration>,
}

/// What a notice IS, which is what decides its colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoticeKind {
    Info,
    Error,
    /// A `config/reload` result (M15).
    Config,
    /// A copy flash (M21).
    Copied,
    /// The OUTPUT of a `/` command. Like an error it has no TTL: a report the user asked for that
    /// fades before it is read was never rendered at all (M27).
    Command,
}
