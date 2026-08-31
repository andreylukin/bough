//! Invariant: this row owns `$BOUGH_HOME/tui.sock` for exactly its own life — bound at `apply` so
//! a bind failure is a loud activation failure (§0.2), removed by the effect's inverse so a
//! disabled row leaves no dead socket behind. The home lock (`crates/bough/src/lock.rs`) is what
//! guarantees at most one process serves a home, so a stale file at bind time is always safe to
//! replace.
//!
//! The row is the SERVER half of the resident/attach transport (§11 "The resident"). The client
//! half is the launcher's `attach` module; the two speak [`proto`] and nothing else.

pub mod proto;
pub mod server;

pub mod invariant;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_commands::{
    no_args, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_tui_shell::Tui;

use crate::server::AttachState;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-attach";

/// The row's config. The socket path defaults to `bough_path("tui.sock")` in the bundle — the
/// launcher's attach flow looks for THAT path, so a patched socket is for tests and swaps, not a
/// second way in.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachConfig {
    /// Where to listen. The launcher's clients connect to the DEFAULT `$BOUGH_HOME/tui.sock`.
    pub socket: PathBuf,
    /// How long a connection may take to say hello before it is dropped.
    #[serde(default = "default_handshake_ms")]
    pub handshake_ms: u64,
}

fn default_handshake_ms() -> u64 {
    10_000
}

/// The row.
pub struct TuiAttachPlugin;

#[async_trait::async_trait]
impl Plugin for TuiAttachPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = AttachConfig;

    fn inject() -> Inject {
        Inject::required(["tui"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.socket.as_os_str().is_empty() {
            return reject("socket must name a path".to_string());
        }
        if cfg.handshake_ms == 0 {
            return reject("handshake_ms must be > 0".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let err = |e: anyhow::Error| PluginError::new(ctx.entry_id().clone(), e);
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        // Bind at APPLY: a path we cannot own is a row that must not claim to be active. The home
        // lock guarantees no live process is behind an existing file, so replacing it is safe.
        match std::fs::remove_file(&cfg.socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(err(anyhow::anyhow!(
                    "could not clear a stale socket at {}: {e}",
                    cfg.socket.display()
                )))
            }
        }
        let listener = tokio::net::UnixListener::bind(&cfg.socket).map_err(|e| {
            err(anyhow::anyhow!(
                "could not bind {}: {e}",
                cfg.socket.display()
            ))
        })?;

        let state = Arc::new(AttachState::default());
        register_detach(&ctx, Arc::clone(&state)).await?;

        let socket_path = cfg.socket.clone();
        let (loop_tui, loop_state) = ((*tui).clone(), Arc::clone(&state));
        let handshake_ms = cfg.handshake_ms;
        ctx.effect_spawn(move |e| async move {
            // Disposal sets the halt flag and AWAITS this task before any inverse runs
            // (`EffectHandle::dispose`), so the flag is observed by POLLING it — an inverse-fired
            // notify would be a message this loop could only read after it had already returned.
            let mut halt_poll = tokio::time::interval(std::time::Duration::from_millis(100));
            halt_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut sessions: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            loop {
                tokio::select! {
                    biased;
                    _ = halt_poll.tick() => {
                        if e.is_halted() {
                            break;
                        }
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let (tui, state) = (loop_tui.clone(), Arc::clone(&loop_state));
                            sessions.retain(|h| !h.is_finished());
                            sessions.push(tokio::spawn(server::session(
                                stream, tui, state, handshake_ms,
                            )));
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "tui-attach: accept failed");
                            // A transient accept error must not spin the loop hot.
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                }
            }
            loop_state.close("bough exited");
            // Disposal AWAITS the sessions (bounded): the client's EXIT frame must actually cross
            // the socket before the launcher's teardown takes the runtime — and every task — down.
            for h in sessions {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(1000), h).await;
            }
            match std::fs::remove_file(&socket_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(error = %e, "tui-attach: could not remove the socket"),
            }
            Ok(())
        });
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// Register `/detach`, if a command registry is bound. ABSENT is headless; an ERROR is a boot
/// failure, never a row that silently registered nothing (§0.2).
async fn register_detach(ctx: &Context, state: Arc<AttachState>) -> Result<(), PluginError> {
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("detach"),
                summary: "let this terminal go; bough keeps running in the background".to_string(),
                usage: "/detach".to_string(),
                args: no_args(),
                scope: CommandScope::Global,
                run: Arc::new(DetachCommand { state }),
            },
        )
        .await?;
    Ok(())
}

struct DetachCommand {
    state: Arc<AttachState>,
}

pub const DETACH_REASON: &str = "detached; bough keeps running — `bough` attaches again";

#[async_trait::async_trait]
impl Command for DetachCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let text = if self.state.detach(0, DETACH_REASON) {
            "detached this terminal; bough keeps running".to_string()
        } else {
            "no terminal is attached; nothing to detach".to_string()
        };
        Ok(CommandOutput {
            text,
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16's lint: no summary this row registers may use the tree's internal vocabulary.
    #[test]
    fn the_detach_summary_is_plain_language() {
        assert_eq!(
            bough_plugin_commands::palette::house_word(
                "let this terminal go; bough keeps running in the background"
            ),
            None
        );
    }

    #[test]
    fn validate_refuses_the_degenerate_values() {
        let ok = AttachConfig {
            socket: PathBuf::from("/tmp/x.sock"),
            handshake_ms: 10,
        };
        assert!(TuiAttachPlugin::validate(&ok).is_ok());
        let empty = AttachConfig {
            socket: PathBuf::new(),
            handshake_ms: 10,
        };
        assert!(TuiAttachPlugin::validate(&empty).is_err());
        let zero = AttachConfig {
            socket: PathBuf::from("/tmp/x.sock"),
            handshake_ms: 0,
        };
        assert!(TuiAttachPlugin::validate(&zero).is_err());
    }
}

bough_kernel::register_plugin!(TuiAttachPlugin);
