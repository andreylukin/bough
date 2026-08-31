//! §11 "The resident", the transport end to end against the REAL binary: a resident boots with
//! the `tui.attach` row, a protocol client attaches and sees the composer, typing crosses the
//! wire, a second attach detaches the first with a named reason, `/detach` lets a client go
//! while the resident keeps running, and the two-press exit tears the whole thing down with an
//! EXIT frame on the socket and a zero on the process.
//!
//! The client here speaks `bough_plugin_tui_attach::proto` directly — the same module the
//! launcher's own client half uses — so what this file proves is the wire, not a mock of it.

use bough_plugin_tui_attach::proto::{self, ClientHello, Exit, ServerHello};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;

/// The smallest tree the transport needs: a ledger, the command registry, the roster, the shell
/// (headless: stdout is a pipe), and the row under test.
const BUNDLE: &str = "\
- id: ledger
  plugin: ledger-memory
  config: {}
- id: commands
  plugin: commands
  config: { prefix: \"/\", suggestions: true }
- id: agents
  plugin: agents
- id: tui
  plugin: tui-shell
  config:
    backend: auto
    size: [80, 24]
    frame_ms: 16
    tick_ms: 250
    theme: dark
    mouse: true
    osc52: true
    clipboard: false
    composer_max_lines: 6
- id: tui.attach
  plugin: tui-attach
  config:
    socket: !!expr 'bough_path(\"tui.sock\")'
    handshake_ms: 5000
";

struct Resident {
    child: std::process::Child,
    home: tempfile::TempDir,
}

impl Resident {
    fn socket(&self) -> std::path::PathBuf {
        self.home.path().join("tui.sock")
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Boot the real binary over the minimal bundle and wait for its socket.
async fn resident() -> Resident {
    let home = tempfile::tempdir().expect("a temp home");
    std::fs::create_dir_all(home.path().join("bundles")).unwrap();
    std::fs::create_dir_all(home.path().join("profiles")).unwrap();
    // Named `bough-tui-app` so boot takes the HOME LOCK, exactly as the shipped profile
    // does — the restart verb finds its target through that lock.
    std::fs::write(home.path().join("bundles/bough-tui-app.yml"), BUNDLE).unwrap();
    std::fs::write(
        home.path().join("profiles/tui.yml"),
        "name: tui\ninvariants: false\nbundles: [bough-tui-app]\n",
    )
    .unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
        .args(["--no-watch"])
        .arg("--root")
        .arg(home.path())
        .env("BOUGH_HOME", home.path())
        .env("HOME", home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the resident");
    let r = Resident { child, home };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !r.socket().exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the resident never bound its socket"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    r
}

/// Attach one protocol client: connect, hello, assert the ack.
async fn attach(r: &Resident) -> UnixStream {
    let mut stream = UnixStream::connect(r.socket()).await.expect("connect");
    let hello = ClientHello {
        version: proto::VERSION,
        cols: 80,
        rows: 24,
    };
    proto::write_frame(
        &mut stream,
        proto::C_HELLO,
        &proto::encode("hello", &hello).unwrap(),
    )
    .await
    .expect("hello out");
    let (tag, payload) = read_one(&mut stream).await.expect("an ack");
    assert_eq!(tag, proto::S_HELLO, "the first frame back is the ack");
    let ack: ServerHello = proto::decode("hello", &payload).expect("a server hello");
    assert_eq!(ack.version, proto::VERSION);
    assert!(ack.mouse, "the config's mouse flag crosses the wire");
    stream
}

async fn read_one<R: AsyncRead + Unpin>(r: &mut R) -> Option<(u8, Vec<u8>)> {
    tokio::time::timeout(std::time::Duration::from_secs(10), proto::read_frame(r))
        .await
        .expect("a frame within ten seconds")
        .expect("an intact frame")
}

/// Accumulate BYTES frames until `needle` shows up in the decoded stream (or panic at the bound).
async fn screen_until<R: AsyncRead + Unpin>(r: &mut R, needle: &str) -> String {
    let mut seen = String::new();
    for _ in 0..200 {
        match read_one(r).await {
            Some((proto::S_BYTES, bytes)) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains(needle) {
                    return seen;
                }
            }
            Some((tag, payload)) => panic!(
                "waiting for {needle:?}, got frame tag {tag}: {:?}",
                String::from_utf8_lossy(&payload)
            ),
            None => panic!("EOF while waiting for {needle:?}; saw: {seen:?}"),
        }
    }
    panic!("{needle:?} never appeared; saw: {seen:?}");
}

/// Read frames until the EXIT frame, skipping BYTES that were already in flight.
async fn exit_frame<R: AsyncRead + Unpin>(r: &mut R) -> Exit {
    for _ in 0..200 {
        match read_one(r).await {
            Some((proto::S_EXIT, payload)) => return proto::decode("exit", &payload).unwrap(),
            Some((proto::S_BYTES, _)) => {}
            Some((tag, _)) => panic!("waiting for EXIT, got frame tag {tag}"),
            None => panic!("EOF while waiting for the EXIT frame"),
        }
    }
    panic!("the EXIT frame never came");
}

async fn send_event<W: AsyncWrite + Unpin>(w: &mut W, ev: Event) {
    proto::write_frame(w, proto::C_EVENT, &proto::encode("event", &ev).unwrap())
        .await
        .expect("event out");
}

fn key(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

/// The whole life of the transport, in the order a user would live it.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_attaches_types_is_stolen_detaches_and_the_exit_reaches_the_wire() {
    let r = resident().await;

    // 1. Attach; the composer's placeholder reaches this side of the wire. The diff skips
    //    unchanged blank cells, so a phrase arrives word by word — assert on the words.
    let mut one = attach(&r).await;
    let seen = screen_until(&mut one, "Type").await;
    assert!(
        seen.contains("message"),
        "the composer placeholder: {seen:?}"
    );

    // 2. Typing crosses: a paste lands in the composer whole and comes back as one painted run.
    send_event(&mut one, Event::Paste("hello attach".to_string())).await;
    screen_until(&mut one, "hello attach").await;

    // 3. A second attach steals, and the first is told why. The composer is SHARED state — the
    //    new client's first full frame carries the very draft the old one typed.
    let mut two = attach(&r).await;
    let stolen = exit_frame(&mut one).await;
    assert!(
        stolen.reason.contains("another bough"),
        "the first client's reason: {}",
        stolen.reason
    );
    assert_eq!(stolen.code, 0);
    screen_until(&mut two, "hello attach").await;

    // 4. `/detach` lets the client go; the resident keeps running. The draft is cleared first —
    //    a slash line only dispatches from the line's start.
    send_event(&mut two, key(KeyCode::Char('u'), KeyModifiers::CONTROL)).await;
    send_event(&mut two, Event::Paste("/detach".to_string())).await;
    send_event(&mut two, key(KeyCode::Enter, KeyModifiers::NONE)).await;
    let detached = exit_frame(&mut two).await;
    assert!(
        detached.reason.contains("detached"),
        "the /detach reason: {}",
        detached.reason
    );

    // 5. …running enough that a third client attaches to the same process.
    let mut three = attach(&r).await;
    screen_until(&mut three, "Type").await;

    // 6. Ctrl+C twice while idle is the two-press exit (B7): the EXIT frame reaches the wire and
    //    the process leaves with a zero.
    send_event(&mut three, key(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;
    screen_until(&mut three, "again to exit").await;
    send_event(&mut three, key(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;
    let goodbye = exit_frame(&mut three).await;
    assert_eq!(goodbye.code, 0, "reason: {}", goodbye.reason);

    let mut r = r;
    let status = tokio::task::spawn_blocking(move || {
        let status = r.child.wait().expect("the resident's exit status");
        drop(r); // after wait: Drop's kill would race an already-reaped pid
        status
    })
    .await
    .unwrap();
    assert!(status.success(), "the resident exits 0 on /quit: {status}");
}

/// The socket is the row's effect: it is gone once the process is.
#[tokio::test(flavor = "multi_thread")]
async fn the_socket_is_removed_when_the_resident_exits() {
    let r = resident().await;
    let mut c = attach(&r).await;
    screen_until(&mut c, "Type").await;
    send_event(&mut c, key(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;
    screen_until(&mut c, "again to exit").await;
    send_event(&mut c, key(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;
    let _ = exit_frame(&mut c).await;
    let mut r = r;
    let _ = r.child.wait();
    assert!(
        !r.socket().exists(),
        "a clean exit takes the socket file with it"
    );
}

/// `bough restart` end to end: the running resident is asked to leave through the same teardown
/// a terminal Ctrl+C takes, the flock's release is what "it left" means, and a fresh resident
/// owns the socket afterwards. The fresh one boots the SHIPPED tree (restart forwards no --root),
/// which the scratch home isolates.
#[tokio::test(flavor = "multi_thread")]
async fn restart_replaces_the_resident_and_a_client_attaches_to_the_new_one() {
    let mut r = resident().await;
    let old_pid = std::fs::read_to_string(r.home.path().join("lock"))
        .expect("the lock file")
        .trim()
        .parse::<i32>()
        .expect("the owner pid");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
        .arg("restart")
        .env("BOUGH_HOME", r.home.path())
        .env("HOME", r.home.path())
        .output()
        .expect("run bough restart");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "restart exits 0: {stderr}");
    assert!(
        stderr.contains(&format!("asked pid {old_pid} to leave")),
        "the old owner is named: {stderr}"
    );

    // The old process is gone. It is this test's own CHILD, so it lingers as a zombie —
    // `kill(pid, 0)` still answers a zombie — until `wait` reaps it; the flock's release
    // already proved the death, the reap is bookkeeping.
    let status = r.child.wait().expect("reap the old resident");
    assert!(
        status.success(),
        "the old resident tore down cleanly: {status}"
    );
    assert_ne!(
        unsafe { libc::kill(old_pid, 0) },
        0,
        "pid {old_pid} must be gone"
    );
    let new_pid = std::fs::read_to_string(r.home.path().join("lock"))
        .expect("the new lock file")
        .trim()
        .parse::<i32>()
        .expect("the new owner pid");
    // The fresh resident is NOT this test's child — `bough restart` spawned and orphaned it —
    // so a panic below this line would leak a live process into the machine. The guard is what
    // cleans it up on EVERY exit, which three leaked residents from this test's own first drafts
    // proved is not a theoretical concern.
    struct Reap(i32);
    impl Drop for Reap {
        fn drop(&mut self) {
            unsafe { libc::kill(self.0, libc::SIGINT) };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while unsafe { libc::kill(self.0, 0) } == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let _reap = Reap(new_pid);
    assert_ne!(new_pid, old_pid, "a fresh process holds the home");
    let mut c = attach(&r).await;
    screen_until(&mut c, "Type").await;
}
