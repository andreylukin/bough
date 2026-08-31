//! Invariant: the CLIENT owns its own terminal. Raw mode, the alt screen and their restore all
//! happen in this process, so a resident that dies — or a socket that tears — can never leave the
//! user's terminal wedged: the guard and the panic hook live where the tty does. What crosses the
//! socket is `bough_plugin_tui_attach::proto` and nothing else; the resident does the composing,
//! this side does the typing and the painting.
//!
//! This is the launcher's transport, not a behaviour switch (§0.1 item 2): a bare `bough` on a
//! tty ATTACHES to the home's resident — spawning one first when none is live — while anything
//! explicit (`--local`, `--resident`, a subcommand, `--check`, `--dump-config`, a non-default
//! profile, a patch layer, `--root`) composes in-process exactly as before.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bough_plugin_tui_attach::proto::{self, ClientHello, Exit, ServerHello};
use futures::StreamExt;
use tokio::net::UnixStream;

/// Where the home's resident listens. The `tui.attach` row's bundle default is the same
/// expression; the two are the one meeting point of client and server.
pub fn socket_path() -> PathBuf {
    bough_util::bough_path("tui.sock")
}

/// How long the client waits — for the resident's hello, and for a freshly spawned resident to
/// bind its socket. A protocol constant, not a tunable: it bounds a wait this module owns.
pub const CLIENT_WAIT_MS: u64 = 15_000;

/// Whether this invocation attaches instead of composing. PURE, so the rule is testable without a
/// terminal: only the bare default invocation attaches; every explicit choice composes in-process.
#[allow(clippy::too_many_arguments)]
pub fn wants_attach(
    tty: bool,
    has_subcommand: bool,
    check: bool,
    dump_config: bool,
    local: bool,
    resident: bool,
    profile_is_default: bool,
    has_patches: bool,
    has_root: bool,
) -> bool {
    tty && !has_subcommand
        && !check
        && !dump_config
        && !local
        && !resident
        && profile_is_default
        && !has_patches
        && !has_root
}

/// Attach to the home's resident, spawning one first when none is live. Returns the process's
/// exit code; every error path restores nothing because nothing was entered yet when it can fail.
pub async fn attach_or_spawn() -> ExitCode {
    let path = socket_path();
    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => match spawn_and_wait(&path).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bough: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    match run_client(stream).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            // Whatever was entered is undone before the error prints (the client's one job).
            bough_plugin_tui_shell::restore_now();
            eprintln!("bough: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Spawn `bough --resident` detached from this terminal and wait for its socket.
async fn spawn_and_wait(path: &Path) -> anyhow::Result<UnixStream> {
    eprintln!("bough: no resident is running; starting one\u{2026}");
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--resident")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group: closing this terminal must not deliver the resident a SIGHUP.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(CLIENT_WAIT_MS);
    loop {
        if let Ok(stream) = UnixStream::connect(path).await {
            return Ok(stream);
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "the resident exited during startup ({status}); \
                 see $BOUGH_HOME/bough.log, or run `bough --local`"
            );
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the resident did not bind {} within {CLIENT_WAIT_MS}ms; \
                 see $BOUGH_HOME/bough.log, or run `bough --local`",
                path.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// One attached session: hello, enter this terminal, pump events out and bytes in, restore, and
/// say why it ended. Returns the code the resident named.
pub async fn run_client(stream: UnixStream) -> anyhow::Result<u8> {
    let (mut read_half, mut write_half) = stream.into_split();

    let (cols, rows) = crossterm::terminal::size()?;
    let hello = ClientHello {
        version: proto::VERSION,
        cols,
        rows,
    };
    proto::write_frame(
        &mut write_half,
        proto::C_HELLO,
        &proto::encode("hello", &hello)?,
    )
    .await?;

    let ack: ServerHello = match tokio::time::timeout(
        std::time::Duration::from_millis(CLIENT_WAIT_MS),
        proto::read_frame(&mut read_half),
    )
    .await
    {
        Ok(Ok(Some((proto::S_HELLO, payload)))) => proto::decode("hello", &payload)?,
        Ok(Ok(Some((proto::S_EXIT, payload)))) => {
            // Refused before it began (a protocol mismatch, a closing listener): the reason is
            // the whole story, and nothing was entered yet.
            let exit: Exit = proto::decode("exit", &payload)?;
            println!("bough: {}", exit.reason);
            return Ok(exit.code);
        }
        Ok(Ok(_)) => anyhow::bail!("the resident answered with something that is not a hello"),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => anyhow::bail!("the resident did not answer the hello within {CLIENT_WAIT_MS}ms"),
    };
    if ack.version != proto::VERSION {
        anyhow::bail!(
            "protocol mismatch: this bough speaks v{}, the resident v{} — rebuild one of them",
            proto::VERSION,
            ack.version
        );
    }

    // From here the terminal is ENTERED and every way out must restore it. The guard's
    // bookkeeping is process-global, so the panic hook and the explicit restore agree.
    let unhook = bough_plugin_tui_shell::install_panic_hook();
    let guard = bough_plugin_tui_shell::TerminalGuard::enter_flags(ack.mouse)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // This terminal's events, straight onto the wire. The task ends when the socket closes under
    // it or stdin does; either way the main loop below is what decides the exit.
    let pump = tokio::spawn(async move {
        let mut events = crossterm::event::EventStream::new();
        while let Some(ev) = events.next().await {
            let Ok(ev) = ev else { break };
            let Ok(payload) = proto::encode("event", &ev) else {
                break;
            };
            if proto::write_frame(&mut write_half, proto::C_EVENT, &payload)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let (farewell, code) = loop {
        match proto::read_frame(&mut read_half).await {
            Ok(Some((proto::S_BYTES, bytes))) => {
                use std::io::Write;
                let mut out = std::io::stdout();
                out.write_all(&bytes)?;
                out.flush()?;
            }
            Ok(Some((proto::S_EXIT, payload))) => {
                let exit: Exit = proto::decode("exit", &payload)?;
                break (exit.reason, exit.code);
            }
            Ok(Some(_)) => {} // an unknown tag from a same-version server: skip, stay attached
            Ok(None) => break ("the resident went away".to_string(), 1),
            Err(e) => break (format!("the connection tore: {e}"), 1),
        }
    };

    pump.abort();
    drop(guard);
    unhook();
    println!("bough: {farewell}");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::wants_attach;

    /// Only the bare default invocation attaches; every explicit choice composes in-process.
    #[test]
    fn only_a_bare_default_invocation_attaches() {
        let bare = wants_attach(true, false, false, false, false, false, true, false, false);
        assert!(bare);
        // No tty: a piped bough composes as before.
        assert!(!wants_attach(
            false, false, false, false, false, false, true, false, false
        ));
        // Every explicit choice, one at a time.
        assert!(
            !wants_attach(true, true, false, false, false, false, true, false, false),
            "subcommand"
        );
        assert!(
            !wants_attach(true, false, true, false, false, false, true, false, false),
            "--check"
        );
        assert!(
            !wants_attach(true, false, false, true, false, false, true, false, false),
            "--dump-config"
        );
        assert!(
            !wants_attach(true, false, false, false, true, false, true, false, false),
            "--local"
        );
        assert!(
            !wants_attach(true, false, false, false, false, true, true, false, false),
            "--resident"
        );
        assert!(
            !wants_attach(true, false, false, false, false, false, false, false, false),
            "--profile"
        );
        assert!(
            !wants_attach(true, false, false, false, false, false, true, true, false),
            "--patch"
        );
        assert!(
            !wants_attach(true, false, false, false, false, false, true, false, true),
            "--root"
        );
    }
}
