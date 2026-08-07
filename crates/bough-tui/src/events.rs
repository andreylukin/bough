//! The SSE subscription: `GET /events`, forever, across server restarts
//! (port of `src/tui/events.ts`).
//!
//! THE INVARIANT THIS HOLDS, and it is the whole design: **`seq` is a dedupe
//! key, not a resume cursor.** There is therefore nothing here that resumes. No
//! `Last-Event-ID` header is sent — the server deliberately emits no `id:`
//! field for exactly this reason — no seq is remembered across a reconnect, and
//! no frame is ever asked for again. What happens on RE-connect instead is that
//! `on_open` fires with `reconnect: true` and the caller re-fetches
//! `GET /sessions/:id` and reconciles by message id. The database is the source
//! of truth; this stream is display transport.
//!
//! Second invariant: **the known-type list is the schema's, never a local
//! copy.** [`known_event_types`] is derived from the frozen
//! `bough_core::schema::events::EVENT_TYPES`, making drift impossible rather
//! than merely unlikely.
//!
//! Third: **no renderer, no terminal, no globals.** `connect_events` is a plain
//! function returning a handle; the fetch and the retry delay are injectable,
//! so the reconnect behaviour is testable with nothing on the network.
//!
//! The envelope IS schema-parsed — this is the one place bytes come off a
//! socket — and a malformed frame is skipped, never fatal.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use futures::stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use bough_core::schema::events::{BoughEvent, EVENT_TYPES};

/// The closed set, straight from the frozen schema. Frames outside it are ignored.
pub fn known_event_types() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| EVENT_TYPES.iter().copied().collect())
}

/// How long to wait before redialing a dropped or refused connection.
pub const RETRY_MS: u64 = 2_000;

/// Parse whole SSE frames out of `buffer` and return the unconsumed tail.
///
/// Pure and exported so framing is testable on strings — including the cases
/// that matter and are easy to get wrong: a frame split across two chunk
/// boundaries, and a comment line (`: ping`, `: connected`), which carries no
/// `event:` and must be skipped without disturbing the buffer.
pub fn parse_frames(buffer: &str, mut emit: impl FnMut(&str, &str)) -> String {
    let mut at = 0usize;
    loop {
        let Some(rel) = buffer[at..].find("\n\n") else {
            return buffer[at..].to_string();
        };
        let end = at + rel;
        let mut frame_type = String::new();
        let mut data = String::new();
        for line in buffer[at..end].split('\n') {
            if let Some(rest) = line.strip_prefix("event:") {
                frame_type = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data:") {
                // A multi-line `data:` is concatenated, per the SSE grammar, with
                // one optional leading space stripped per line. The server writes
                // one line (JSON escapes every newline), but the parser must not
                // depend on that.
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
            // Anything else — a comment (`:`), a `retry:` — is not ours to interpret.
        }
        if !frame_type.is_empty() && !data.is_empty() {
            emit(&frame_type, &data);
        }
        at = end + 2;
    }
}

/// What one decoded frame turned out to be.
#[derive(Debug)]
pub enum FrameOutcome {
    Event(BoughEvent),
    /// Unknown type, non-JSON data, or a payload that is not a valid envelope.
    /// Skipped, never fatal — reported so a schema drift is visible instead of
    /// silently dropping display data.
    Bad {
        r#type: String,
        data: String,
        error: Option<String>,
    },
}

/// The one place bytes become state. Check the type against the frozen set,
/// parse the JSON, parse the envelope — in that order, exactly as the TS drain
/// loop does — and never trust any of it.
pub fn decode_frame(frame_type: &str, data: &str) -> FrameOutcome {
    if !known_event_types().contains(frame_type) {
        return FrameOutcome::Bad {
            r#type: frame_type.to_string(),
            data: data.to_string(),
            error: None,
        };
    }
    let payload: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(err) => {
            return FrameOutcome::Bad {
                r#type: frame_type.to_string(),
                data: data.to_string(),
                error: Some(err.to_string()),
            }
        }
    };
    match serde_json::from_value::<BoughEvent>(payload) {
        Ok(event) => FrameOutcome::Event(event),
        Err(err) => FrameOutcome::Bad {
            r#type: frame_type.to_string(),
            data: data.to_string(),
            error: Some(err.to_string()),
        },
    }
}

// ---- the connection loop ----------------------------------------------------

/// A live SSE body: chunks of bytes, or the transport's error text.
pub type SseBody = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;
type SseFuture = Pin<Box<dyn Future<Output = Result<SseBody, String>> + Send>>;
/// The injectable dial. Headers are what the request will carry — a test
/// asserts they never include `last-event-id`. A non-2xx response is an `Err`
/// with `GET <url>: <status>` (the TS loop throws the same text).
pub type SseFetchFn = Arc<dyn Fn(String, Vec<(String, String)>) -> SseFuture + Send + Sync>;

/// The one header the dial sends. `seq` is not a resume cursor: there is no
/// `Last-Event-ID` here, by design, and a test pins that.
pub fn request_headers() -> Vec<(String, String)> {
    vec![("accept".to_string(), "text/event-stream".to_string())]
}

fn reqwest_sse_fetch() -> SseFetchFn {
    let client = reqwest::Client::new();
    Arc::new(move |url: String, headers: Vec<(String, String)>| {
        let client = client.clone();
        Box::pin(async move {
            let mut builder = client.get(&url);
            for (k, v) in &headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            let res = builder.send().await.map_err(|e| e.to_string())?;
            if !res.status().is_success() {
                return Err(format!("GET {url}: {}", res.status().as_u16()));
            }
            let stream = res
                .bytes_stream()
                .map(|chunk| chunk.map(|b| b.to_vec()).map_err(|e| e.to_string()));
            Ok(Box::pin(stream) as SseBody)
        })
    })
}

pub struct OpenInfo {
    /// False for the very first open (the caller's state is already fresh) and
    /// true afterwards — the signal to RE-FETCH, because whatever was published
    /// while the connection was down is gone for good.
    pub reconnect: bool,
    pub attempt: u64,
}

pub struct CloseInfo {
    /// The transport's failure text; `None` for a clean server-side EOF.
    pub error: Option<String>,
}

pub struct BadFrame {
    pub r#type: String,
    pub data: String,
    pub error: Option<String>,
}

pub struct EventStreamOptions {
    /// The stream URL. Absent = built from `base` and `session_id`.
    pub url: Option<String>,
    /// Origin, when `url` is not given.
    pub base: Option<String>,
    /// Scope the stream to one session. Un-scoped events are delivered regardless.
    pub session_id: Option<String>,
    /// Every well-formed, known-type event, in arrival order.
    pub on_event: Box<dyn Fn(BoughEvent) + Send + Sync>,
    pub on_open: Option<Box<dyn Fn(OpenInfo) + Send + Sync>>,
    /// The stream went down. Always followed by a retry unless the handle was closed.
    pub on_close: Option<Box<dyn Fn(CloseInfo) + Send + Sync>>,
    pub on_bad_frame: Option<Box<dyn Fn(BadFrame) + Send + Sync>>,
    pub retry_ms: Option<u64>,
    /// Injected by tests; absent = reqwest.
    pub fetch_fn: Option<SseFetchFn>,
}

/// The live handle. `close()` is idempotent; `done()` resolves once the loop
/// has stopped (tests await it; the TUI ignores it).
pub struct EventStream {
    connected: Arc<AtomicBool>,
    opens: Arc<AtomicU64>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl EventStream {
    /// Is a connection live right now? Drives the disconnected indicator.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    /// How many times the stream has come up. 1 = the first open.
    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::SeqCst)
    }
    /// Stop reconnecting and release the connection. Idempotent.
    pub fn close(&self) {
        self.cancel.cancel();
    }
    /// Resolves once the loop has stopped.
    pub async fn done(self) {
        let _ = self.task.await;
    }
}

/// Open the stream and keep it open.
///
/// The loop is deliberately shaped so that "connected" is only true while bytes
/// can actually arrive: it is set after the dial succeeds and cleared on every
/// exit path, including the one where the server closed the body cleanly. A
/// client that showed "connected" while its reader was at end-of-stream would
/// hide precisely the outage the reconnect fetch exists to repair.
pub fn connect_events(options: EventStreamOptions) -> EventStream {
    let retry_ms = options.retry_ms.unwrap_or(RETRY_MS);
    let fetch = options.fetch_fn.unwrap_or_else(reqwest_sse_fetch);
    let url = options.url.unwrap_or_else(|| {
        let base = options.base.unwrap_or_default();
        match &options.session_id {
            Some(sid) => format!("{base}/events?sessionId={}", encode_uri_component(sid)),
            None => format!("{base}/events"),
        }
    });

    let connected = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicU64::new(0));
    let cancel = CancellationToken::new();

    let task = {
        let connected = connected.clone();
        let opens = opens.clone();
        let cancel = cancel.clone();
        let on_event = options.on_event;
        let on_open = options.on_open;
        let on_close = options.on_close;
        let on_bad_frame = options.on_bad_frame;
        tokio::spawn(async move {
            while !cancel.is_cancelled() {
                let mut error: Option<String> = None;
                let dial = tokio::select! {
                    _ = cancel.cancelled() => break,
                    dialed = fetch(url.clone(), request_headers()) => dialed,
                };
                match dial {
                    Err(err) => error = Some(err),
                    Ok(mut body) => {
                        connected.store(true, Ordering::SeqCst);
                        let attempt = opens.fetch_add(1, Ordering::SeqCst) + 1;
                        if let Some(cb) = &on_open {
                            cb(OpenInfo {
                                reconnect: attempt > 1,
                                attempt,
                            });
                        }
                        // Byte carry, cut only at frame boundaries (`\n\n` cannot
                        // sit inside a multi-byte character), so a UTF-8 code
                        // point split across chunks survives.
                        let mut carry: Vec<u8> = Vec::new();
                        loop {
                            let chunk = tokio::select! {
                                _ = cancel.cancelled() => break,
                                chunk = body.next() => chunk,
                            };
                            match chunk {
                                None => break, // clean EOF: still a disconnect
                                Some(Err(err)) => {
                                    error = Some(err);
                                    break;
                                }
                                Some(Ok(bytes)) => {
                                    carry.extend_from_slice(&bytes);
                                    if let Some(pos) = find_last_frame_end(&carry) {
                                        let complete: Vec<u8> = carry.drain(..pos).collect();
                                        let text = String::from_utf8_lossy(&complete);
                                        let tail =
                                            parse_frames(&text, |t, d| match decode_frame(t, d) {
                                                FrameOutcome::Event(event) => on_event(event),
                                                FrameOutcome::Bad {
                                                    r#type,
                                                    data,
                                                    error,
                                                } => {
                                                    if let Some(cb) = &on_bad_frame {
                                                        cb(BadFrame {
                                                            r#type,
                                                            data,
                                                            error,
                                                        });
                                                    }
                                                }
                                            });
                                        debug_assert!(tail.is_empty());
                                    }
                                }
                            }
                        }
                    }
                }

                if connected.swap(false, Ordering::SeqCst) {
                    if let Some(cb) = &on_close {
                        cb(CloseInfo { error });
                    }
                }
                if cancel.is_cancelled() {
                    break;
                }
                // Abortable delay: resolves early — never errors — on close.
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(retry_ms)) => {}
                }
            }
            connected.store(false, Ordering::SeqCst);
        })
    };

    EventStream {
        connected,
        opens,
        cancel,
        task,
    }
}

/// Index one past the last `\n\n` in `buf`, if any complete frame is buffered.
fn find_last_frame_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).rposition(|w| w == b"\n\n").map(|i| i + 2)
}

/// `encodeURIComponent`, for the `?sessionId=` the TS client builds the same way.
fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    // ---- framing (pure) ------------------------------------------------------

    #[test]
    fn parse_frames_returns_the_unconsumed_tail_so_a_split_frame_survives() {
        let mut seen: Vec<(String, String)> = Vec::new();
        let tail = parse_frames(
            "event: message.delta\ndata: {\"a\":1}\n\nevent: turn.fi",
            |t, d| seen.push((t.to_string(), d.to_string())),
        );
        assert_eq!(
            seen,
            vec![("message.delta".to_string(), "{\"a\":1}".to_string())]
        );
        assert_eq!(tail, "event: turn.fi");

        let tail = parse_frames(&format!("{tail}nished\ndata: {{\"b\":2}}\n\n"), |t, d| {
            seen.push((t.to_string(), d.to_string()))
        });
        assert_eq!(
            seen[1],
            ("turn.finished".to_string(), "{\"b\":2}".to_string())
        );
        assert_eq!(tail, "");
    }

    #[test]
    fn comment_lines_are_skipped_without_disturbing_the_stream() {
        let mut seen: Vec<String> = Vec::new();
        let tail = parse_frames(
            ": connected\n\n: ping\n\nevent: tool.log\ndata: {\"line\":\"x\"}\n\n",
            |t, _| seen.push(t.to_string()),
        );
        assert_eq!(seen, vec!["tool.log"]);
        assert_eq!(tail, "");
    }

    #[test]
    fn multi_line_data_concatenates_with_one_leading_space_stripped_per_line() {
        let mut seen: Vec<(String, String)> = Vec::new();
        parse_frames("event: tool.log\ndata: {\"a\":\ndata:  1}\n\n", |t, d| {
            seen.push((t.to_string(), d.to_string()))
        });
        assert_eq!(
            seen,
            vec![("tool.log".to_string(), "{\"a\": 1}".to_string())]
        );
    }

    #[test]
    fn the_known_type_list_is_the_schemas_so_it_cannot_drift() {
        let mut ours: Vec<&str> = known_event_types().iter().copied().collect();
        let mut schema: Vec<&str> = EVENT_TYPES.to_vec();
        ours.sort_unstable();
        schema.sort_unstable();
        assert_eq!(ours, schema);
    }

    // ---- the loop, against an injected dial ----------------------------------

    /// A scripted dial: each entry is one connection attempt. Records the
    /// headers of every attempt.
    struct Fake {
        attempts: Mutex<Vec<Attempt>>,
        headers_seen: Mutex<Vec<Vec<(String, String)>>>,
    }
    enum Attempt {
        Refused(String),
        /// Chunks delivered, then the stream stays open until dropped.
        Chunks(Vec<Vec<u8>>),
        /// Chunks delivered, then a clean EOF.
        ChunksThenEof(Vec<Vec<u8>>),
    }

    fn fake_fetch(attempts: Vec<Attempt>) -> (SseFetchFn, Arc<Fake>) {
        let fake = Arc::new(Fake {
            attempts: Mutex::new(attempts),
            headers_seen: Mutex::new(Vec::new()),
        });
        let f = fake.clone();
        let fetch: SseFetchFn = Arc::new(move |_url, headers| {
            f.headers_seen.lock().unwrap().push(headers);
            let next = {
                let mut a = f.attempts.lock().unwrap();
                if a.is_empty() {
                    None
                } else {
                    Some(a.remove(0))
                }
            };
            Box::pin(async move {
                match next {
                    None => {
                        // Script exhausted: hang forever (close() unblocks it).
                        futures::future::pending::<()>().await;
                        unreachable!()
                    }
                    Some(Attempt::Refused(err)) => Err(err),
                    Some(Attempt::Chunks(chunks)) => {
                        let s = futures::stream::iter(chunks.into_iter().map(Ok))
                            .chain(futures::stream::pending());
                        Ok(Box::pin(s) as SseBody)
                    }
                    Some(Attempt::ChunksThenEof(chunks)) => {
                        let s = futures::stream::iter(chunks.into_iter().map(Ok));
                        Ok(Box::pin(s) as SseBody)
                    }
                }
            })
        });
        (fetch, fake)
    }

    async fn until(mut check: impl FnMut() -> bool, what: &str) {
        for _ in 0..500 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    #[tokio::test]
    async fn events_flow_parsed_and_the_request_never_asks_to_resume() {
        let frame = "event: message.delta\ndata: {\"type\":\"message.delta\",\"sessionId\":\"s1\",\"seq\":1,\"ts\":9,\"data\":{\"messageId\":\"m\",\"delta\":\"hi\"}}\n\n";
        let (fetch, fake) = fake_fetch(vec![Attempt::Chunks(vec![frame.as_bytes().to_vec()])]);
        let received: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let r = received.clone();
        let stream = connect_events(EventStreamOptions {
            url: Some("http://127.0.0.1:4321/events".into()),
            base: None,
            session_id: None,
            on_event: Box::new(move |e| r.lock().unwrap().push(e)),
            on_open: None,
            on_close: None,
            on_bad_frame: None,
            retry_ms: Some(0),
            fetch_fn: Some(fetch),
        });

        until(
            || received.lock().unwrap().len() == 1,
            "the event to arrive",
        )
        .await;
        {
            let got = received.lock().unwrap();
            assert_eq!(got[0].seq, 1);
            assert_eq!(got[0].session_id.as_deref(), Some("s1"));
        }
        // seq is not a resume cursor: the dial carries accept and NOTHING else.
        {
            let headers = fake.headers_seen.lock().unwrap();
            assert_eq!(
                headers[0],
                vec![("accept".to_string(), "text/event-stream".to_string())]
            );
            assert!(
                !headers[0]
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("last-event-id")),
                "seq is not a resume cursor"
            );
        }

        assert!(stream.connected());
        stream.close();
        stream.done().await;
    }

    #[tokio::test]
    async fn on_open_reports_a_reconnect_which_is_what_triggers_the_refetch() {
        // First dial: the server is not up yet. Then a good connection that the
        // server closes under the client, then a third that stays up.
        let (fetch, _fake) = fake_fetch(vec![
            Attempt::Refused("Connection refused".into()),
            Attempt::ChunksThenEof(vec![b": connected\n\n".to_vec()]),
            Attempt::Chunks(vec![b": connected\n\n".to_vec()]),
        ]);
        let opens: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let closes = Arc::new(AtomicU64::new(0));
        let o = opens.clone();
        let c = closes.clone();
        let stream = connect_events(EventStreamOptions {
            url: Some("http://127.0.0.1:4321/events".into()),
            base: None,
            session_id: None,
            on_event: Box::new(|_| {}),
            on_open: Some(Box::new(move |info| o.lock().unwrap().push(info.reconnect))),
            on_close: Some(Box::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            })),
            on_bad_frame: None,
            retry_ms: Some(0),
            fetch_fn: Some(fetch),
        });

        until(|| opens.lock().unwrap().len() == 2, "the redial").await;
        // A failed dial is not an open: nothing was missed yet. The second open
        // must announce itself as a reconnect — that flag triggers the resync.
        assert_eq!(*opens.lock().unwrap(), vec![false, true]);
        assert_eq!(closes.load(Ordering::SeqCst), 1);

        stream.close();
        stream.done().await;
    }

    #[tokio::test]
    async fn an_unknown_or_malformed_frame_is_skipped_and_the_stream_survives_it() {
        // A server ahead of this client, a truncated payload, an envelope that
        // is not one — then a perfectly good event.
        let chunks: Vec<Vec<u8>> = vec![
            b"event: session.teleported\ndata: {}\n\n".to_vec(),
            b"event: tool.log\ndata: {not json\n\n".to_vec(),
            b"event: tool.log\ndata: {\"seq\":\"one\"}\n\n".to_vec(),
            b"event: tool.log\ndata: {\"type\":\"tool.log\",\"seq\":4,\"ts\":9,\"data\":{\"messageId\":\"m\",\"callId\":\"c\",\"line\":\"ok\"}}\n\n".to_vec(),
        ];
        let (fetch, _fake) = fake_fetch(vec![Attempt::Chunks(chunks)]);
        let received: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let r = received.clone();
        let b = bad.clone();
        let stream = connect_events(EventStreamOptions {
            url: Some("http://127.0.0.1:4321/events".into()),
            base: None,
            session_id: None,
            on_event: Box::new(move |e| r.lock().unwrap().push(e)),
            on_open: None,
            on_close: None,
            on_bad_frame: Some(Box::new(move |f| b.lock().unwrap().push(f.r#type))),
            retry_ms: Some(0),
            fetch_fn: Some(fetch),
        });

        until(
            || received.lock().unwrap().len() == 1,
            "the good event to arrive",
        )
        .await;
        assert_eq!(
            *bad.lock().unwrap(),
            vec!["session.teleported", "tool.log", "tool.log"]
        );
        assert_eq!(received.lock().unwrap()[0].seq, 4);

        stream.close();
        stream.done().await;
    }

    #[tokio::test]
    async fn connected_clears_on_a_clean_eof_and_close_stops_the_redial_loop() {
        let (fetch, fake) = fake_fetch(vec![Attempt::ChunksThenEof(vec![
            b": connected\n\n".to_vec()
        ])]);
        let stream = connect_events(EventStreamOptions {
            url: Some("http://127.0.0.1:4321/events".into()),
            base: None,
            session_id: None,
            on_event: Box::new(|_| {}),
            on_open: None,
            on_close: None,
            on_bad_frame: None,
            retry_ms: Some(0),
            fetch_fn: Some(fetch),
        });
        // The EOF drops the connection and the loop redials (script exhausted →
        // the second dial hangs). `connected` must be false while it does.
        until(
            || fake.headers_seen.lock().unwrap().len() >= 2,
            "the redial after a clean EOF",
        )
        .await;
        assert!(!stream.connected());
        assert_eq!(stream.opens(), 1);
        stream.close();
        stream.done().await;
    }

    #[tokio::test]
    async fn the_scoped_url_is_built_like_the_ts_client_builds_it() {
        let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = recorded.clone();
        // Records the URL then hangs; close() unblocks it.
        let recording: SseFetchFn = Arc::new(move |url, _headers| {
            rec.lock().unwrap().push(url);
            Box::pin(async move {
                futures::future::pending::<()>().await;
                unreachable!()
            })
        });
        let stream = connect_events(EventStreamOptions {
            url: None,
            base: Some("http://127.0.0.1:4321".into()),
            session_id: Some("a b".into()),
            on_event: Box::new(|_| {}),
            on_open: None,
            on_close: None,
            on_bad_frame: None,
            retry_ms: Some(0),
            fetch_fn: Some(recording),
        });
        until(|| !recorded.lock().unwrap().is_empty(), "the dial").await;
        assert_eq!(
            recorded.lock().unwrap()[0],
            "http://127.0.0.1:4321/events?sessionId=a%20b"
        );
        stream.close();
        stream.done().await;
    }
}
