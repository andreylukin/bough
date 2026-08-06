//! `GET /events[?sessionId=]` — the SSE stream every client watches (port of
//! `src/server/events.ts`).
//!
//! **`seq` is a dedupe key, not a resume cursor.** It is process-monotonic and
//! resets on server restart, so nothing here replays, buffers, or accepts a
//! cursor — and **no frame carries an SSE `id:` field**: `id:` is precisely
//! the resume mechanism, and emitting one would advertise a guarantee this
//! server cannot keep. A reconnecting client re-fetches `GET /sessions/:id`
//! and reconciles by message id.
//!
//! **A connection that goes away releases its bus subscription, always.** The
//! subscription is held by a guard owned by the response body's stream;
//! dropping the body — stream cancel, request abort, failed write, whatever
//! wins — unsubscribes exactly once. Teardown is idempotent by construction.
//!
//! **`?sessionId=` scopes the stream, but an event with no `sessionId` is
//! global and always delivered.** Filtering does NOT resolve lineage — a
//! subagent's events go out under its own session id. That keeps this handler
//! free of database access: it is tested with a bus and nothing else.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::response::Response;
use futures::Stream;
use tokio::sync::mpsc;

use bough_core::bus::Bus;
use bough_core::schema::events::BoughEvent;

use crate::http::{handler, Handler, HandlerResult};

/// How often a comment line keeps the connection warm. On loopback its real
/// job is to notice a dead peer: a write to a closed socket surfaces the
/// disconnect the abort may not have reported yet.
pub const HEARTBEAT_MS: u64 = 15_000;

/// The comment line written once the subscription is live, so a client sees
/// bytes immediately.
pub const CONNECTED_FRAME: &str = ": connected\n\n";
/// The keep-alive. A comment, so every SSE client ignores it without a case
/// for it.
pub const HEARTBEAT_FRAME: &str = ": ping\n\n";

/// Which failure phase a stream error was reported from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamErrorPhase {
    /// A defect: an event whose `data` cannot be JSON-encoded. Logged, and
    /// that one event skipped — one malformed payload must not take down a
    /// connection that is otherwise fine.
    Serialize,
    /// Ordinary: the peer closed before teardown ran. Silent by default.
    Enqueue,
}

pub type StreamErrorHook = Arc<dyn Fn(&str, StreamErrorPhase) + Send + Sync>;

#[derive(Default)]
pub struct EventsOptions {
    /// Heartbeat period in ms. `Some(0)` disables it — used by tests that
    /// count frames. Absent = [`HEARTBEAT_MS`].
    pub heartbeat_ms: Option<u64>,
    pub on_stream_error: Option<StreamErrorHook>,
}

/// Does an event with `session_id` reach a stream opened with `filter`?
///
/// Pure, and exported so the rule is testable without a stream: no filter
/// passes everything, and an event with no session id is global and passes
/// regardless.
pub fn passes_filter(session_id: Option<&str>, filter: Option<&str>) -> bool {
    match (filter, session_id) {
        (None, _) => true,
        (_, None) => true,
        (Some(f), Some(s)) => s == f,
    }
}

/// One named SSE frame: `event:` carries the type (clients attach one listener
/// per event name) and the full stamped envelope — `seq` and `ts` included —
/// is the `data:` payload. A single `data:` line is safe for any payload:
/// JSON escapes every newline.
pub fn frame(event: &BoughEvent) -> Result<String, serde_json::Error> {
    Ok(format!(
        "event: {}\ndata: {}\n\n",
        event.r#type.as_str(),
        serde_json::to_string(event)?
    ))
}

/// Unsubscribes on drop — the one teardown, reached from whichever trigger
/// drops the body first, and idempotent because drop runs once.
struct Unsubscriber {
    bus: Arc<Bus>,
    id: u64,
}

impl Drop for Unsubscriber {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.id);
    }
}

/// The response body: drains the per-connection unbounded channel and holds
/// the subscription guard so dropping the body releases the bus slot.
struct SseStream {
    rx: mpsc::UnboundedReceiver<String>,
    _unsub: Unsubscriber,
}

impl Stream for SseStream {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(|s| Ok(Bytes::from(s))))
    }
}

/// The `sessionId` query parameter, if present. Session ids are opaque tokens
/// used verbatim; simple form splitting is the whole grammar.
fn session_filter(query: Option<&str>) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("sessionId=") {
            return Some(v.to_string());
        }
    }
    None
}

/// Build the `/events` handler. [`events`] is the production instance; a test
/// builds its own to disable or shrink the heartbeat.
pub fn create_events_handler(options: EventsOptions) -> Handler {
    let heartbeat_ms = options.heartbeat_ms.unwrap_or(HEARTBEAT_MS);
    let on_stream_error: StreamErrorHook = options.on_stream_error.unwrap_or_else(|| {
        Arc::new(|err: &str, phase| {
            // A dead peer is not news; an unencodable payload is.
            if phase == StreamErrorPhase::Serialize {
                tracing::error!("events: undeliverable event: {err}");
            }
        })
    });

    handler(move |req, ctx, _params| {
        let filter = session_filter(req.uri().query());
        let on_stream_error = on_stream_error.clone();
        async move {
            let (tx, rx) = mpsc::unbounded_channel::<String>();

            // The preamble goes in before the subscription so the client sees
            // bytes immediately.
            let _ = tx.send(CONNECTED_FRAME.to_string());

            let listener_tx = tx.clone();
            let listener_filter = filter.clone();
            let id = ctx.bus.subscribe(Arc::new(move |event: &BoughEvent| {
                if !passes_filter(event.session_id.as_deref(), listener_filter.as_deref()) {
                    return;
                }
                match frame(event) {
                    Ok(text) => {
                        // A send into a dropped receiver is the peer being
                        // gone; the guard's drop is what actually releases us.
                        let _ = listener_tx.send(text);
                    }
                    Err(e) => {
                        // Skip the event, keep the connection. Reported,
                        // never swallowed.
                        on_stream_error(&e.to_string(), StreamErrorPhase::Serialize);
                    }
                }
            }));

            if heartbeat_ms > 0 {
                let hb_tx = tx.clone();
                tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_millis(heartbeat_ms));
                    interval.tick().await; // the immediate first tick is not a heartbeat
                    loop {
                        interval.tick().await;
                        // The receiver dropping is the disconnect signal; the
                        // task ends with it, so a tick after teardown is inert.
                        if hb_tx.send(HEARTBEAT_FRAME.to_string()).is_err() {
                            break;
                        }
                    }
                });
            }

            let stream = SseStream { rx, _unsub: Unsubscriber { bus: ctx.bus.clone(), id } };
            let res: HandlerResult = Ok(Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                // No caching and no buffering: a proxy that holds frames back
                // turns a live stream into a batch delivered at close.
                .header("cache-control", "no-cache, no-transform")
                .header("connection", "keep-alive")
                .body(Body::from_stream(stream))
                .expect("static response parts"));
            res
        }
    })
}

/// The production handler, wired into the route table in `app.rs`.
pub fn events() -> Handler {
    create_events_handler(EventsOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testutil;
    use bough_core::schema::events::{EventInput, EventType};
    use futures::StreamExt;
    use serde_json::json;

    fn sample(session_id: Option<&str>) -> EventInput {
        EventInput {
            r#type: EventType::MessageDelta,
            session_id: session_id.map(|s| s.to_string()),
            data: json!({"messageId": "m1", "delta": "hi"}),
        }
    }

    /// Frame-at-a-time reader over an SSE response body.
    struct Sse {
        stream: axum::body::BodyDataStream,
        buffer: String,
    }

    impl Sse {
        fn new(res: Response) -> Sse {
            Sse { stream: res.into_body().into_data_stream(), buffer: String::new() }
        }

        /// The next complete frame, delimiter included. Panics if the stream
        /// ends first.
        async fn next(&mut self) -> String {
            loop {
                if let Some(end) = self.buffer.find("\n\n") {
                    let one = self.buffer[..end + 2].to_string();
                    self.buffer = self.buffer[end + 2..].to_string();
                    return one;
                }
                let chunk = self
                    .stream
                    .next()
                    .await
                    .expect("stream ended before a frame arrived")
                    .expect("body chunk");
                self.buffer.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        }
    }

    async fn open(fx: &testutil::Fixture, opts: EventsOptions, path: &str) -> Sse {
        let h = create_events_handler(opts);
        let res = h(testutil::get(path), fx.ctx.clone(), Default::default()).await.unwrap();
        assert_eq!(res.status(), 200);
        Sse::new(res)
    }

    fn no_heartbeat() -> EventsOptions {
        EventsOptions { heartbeat_ms: Some(0), on_stream_error: None }
    }

    #[tokio::test]
    async fn opens_with_a_comment_frame_and_the_sse_content_type() {
        let fx = testutil::fixture_bare();
        let h = create_events_handler(no_heartbeat());
        let res = h(testutil::get("/events"), fx.ctx.clone(), Default::default())
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("content-type").unwrap(), "text/event-stream");
        assert!(res
            .headers()
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("no-cache"));
        let mut sse = Sse::new(res);
        assert_eq!(sse.next().await, CONNECTED_FRAME);
    }

    #[tokio::test]
    async fn frames_each_event_as_event_type_plus_one_data_line() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await; // the preamble

        let published = fx.ctx.bus.publish(sample(Some("s1")));
        let got = sse.next().await;
        assert_eq!(
            got,
            format!(
                "event: message.delta\ndata: {}\n\n",
                serde_json::to_string(&published).unwrap()
            )
        );

        // The payload is the whole stamped envelope, seq and ts included.
        let lines: Vec<&str> = got.trim_end().split('\n').collect();
        assert_eq!(lines[0], "event: message.delta");
        let parsed: serde_json::Value =
            serde_json::from_str(&lines[1]["data: ".len()..]).unwrap();
        assert_eq!(parsed["type"], "message.delta");
        assert_eq!(parsed["sessionId"], "s1");
        assert_eq!(parsed["seq"], published.seq);
        assert_eq!(parsed["ts"], published.ts);
        assert_eq!(parsed["data"], json!({"messageId": "m1", "delta": "hi"}));
    }

    #[tokio::test]
    async fn never_emits_an_sse_id_field() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await;

        fx.ctx.bus.publish(sample(Some("s1")));
        let got = sse.next().await;
        for line in got.split('\n') {
            assert!(
                !line.starts_with("id:"),
                "frame carries a resume cursor: {got:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_multi_line_payload_stays_on_one_data_line() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await;

        fx.ctx.bus.publish(EventInput {
            r#type: EventType::ToolLog,
            session_id: Some("s1".into()),
            data: json!({"messageId": "m1", "callId": "c1", "line": "line one\nline two\n\nline three"}),
        });
        let got = sse.next().await;
        let lines: Vec<&str> = got.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 2, "frame split across lines: {got:?}");
        let parsed: serde_json::Value =
            serde_json::from_str(&lines[1]["data: ".len()..]).unwrap();
        assert_eq!(parsed["data"]["line"], "line one\nline two\n\nline three");
    }

    #[tokio::test]
    async fn frames_every_declared_event_type() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await;

        for (t, name) in [
            (EventType::SessionCreated, "session.created"),
            (EventType::AskQuestion, "ask.question"),
            (EventType::WorkflowLog, "workflow.log"),
        ] {
            fx.ctx.bus.publish(EventInput {
                r#type: t,
                session_id: Some("s1".into()),
                data: json!({}),
            });
            let got = sse.next().await;
            assert!(got.starts_with(&format!("event: {name}\n")), "{got:?}");
        }
    }

    #[test]
    fn passes_filter_no_filter_passes_everything_and_global_always_passes() {
        assert!(passes_filter(Some("s1"), None));
        assert!(passes_filter(Some("s2"), None));
        assert!(passes_filter(None, None));

        assert!(passes_filter(Some("s1"), Some("s1")));
        assert!(!passes_filter(Some("s2"), Some("s1")));
        // The rule the endpoint exists to protect: un-scoped events are never
        // dropped.
        assert!(passes_filter(None, Some("s1")));
    }

    #[tokio::test]
    async fn session_filter_drops_other_sessions_but_keeps_global_events() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events?sessionId=s1").await;
        sse.next().await;

        fx.ctx.bus.publish(sample(Some("s2"))); // other session — must not appear
        let global = fx.ctx.bus.publish(sample(None)); // no session — must appear
        let mine = fx.ctx.bus.publish(sample(Some("s1")));

        let first = sse.next().await;
        let seq_of = |frame: &str| -> u64 {
            let line = frame.trim_end().split('\n').nth(1).unwrap();
            serde_json::from_str::<serde_json::Value>(&line["data: ".len()..]).unwrap()["seq"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(seq_of(&first), global.seq);
        let second = sse.next().await;
        assert_eq!(seq_of(&second), mine.seq);
    }

    #[tokio::test]
    async fn an_unfiltered_stream_receives_every_session() {
        let fx = testutil::fixture_bare();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await;

        let a = fx.ctx.bus.publish(sample(Some("s1")));
        let b = fx.ctx.bus.publish(sample(Some("s2")));
        let seq_of = |frame: &str| -> u64 {
            let line = frame.trim_end().split('\n').nth(1).unwrap();
            serde_json::from_str::<serde_json::Value>(&line["data: ".len()..]).unwrap()["seq"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(seq_of(&sse.next().await), a.seq);
        assert_eq!(seq_of(&sse.next().await), b.seq);
    }

    #[tokio::test(start_paused = true)]
    async fn writes_a_comment_heartbeat_on_each_tick() {
        let fx = testutil::fixture_bare();
        let mut sse = open(
            &fx,
            EventsOptions { heartbeat_ms: Some(15_000), on_stream_error: None },
            "/events",
        )
        .await;
        assert_eq!(sse.next().await, CONNECTED_FRAME);

        tokio::time::advance(std::time::Duration::from_millis(15_000)).await;
        assert_eq!(sse.next().await, HEARTBEAT_FRAME);
        tokio::time::advance(std::time::Duration::from_millis(15_000)).await;
        assert_eq!(sse.next().await, HEARTBEAT_FRAME);
    }

    #[tokio::test(start_paused = true)]
    async fn a_heartbeat_tick_after_teardown_is_inert() {
        let fx = testutil::fixture_bare();
        let base = fx.ctx.bus.size(); // the fixture's own event collector
        let sse = open(
            &fx,
            EventsOptions { heartbeat_ms: Some(15_000), on_stream_error: None },
            "/events",
        )
        .await;
        drop(sse); // disconnect
        tokio::time::advance(std::time::Duration::from_millis(30_000)).await;
        tokio::task::yield_now().await;
        assert_eq!(fx.ctx.bus.size(), base);
    }

    #[tokio::test]
    async fn n_connect_disconnect_cycles_leave_no_listener_leak() {
        let fx = testutil::fixture_bare();
        assert_eq!(fx.ctx.bus.size(), 1); // the fixture's own event collector

        for i in 0..50 {
            let mut sse = open(&fx, no_heartbeat(), "/events").await;
            sse.next().await;
            assert_eq!(fx.ctx.bus.size(), 2, "cycle {i}: exactly one subscriber while open");

            fx.ctx.bus.publish(sample(Some("s1")));
            sse.next().await;

            drop(sse);
            assert_eq!(fx.ctx.bus.size(), 1, "cycle {i}: the subscriber must be released");
        }

        // Publishing with every client gone neither panics nor delivers anywhere.
        fx.ctx.bus.publish(sample(Some("s1")));
        assert_eq!(fx.ctx.bus.size(), 1);
    }

    #[tokio::test]
    async fn concurrent_streams_unsubscribe_independently() {
        let fx = testutil::fixture_bare();
        let base = fx.ctx.bus.size();

        let mut open_streams = Vec::new();
        for _ in 0..5 {
            let mut sse = open(&fx, no_heartbeat(), "/events").await;
            sse.next().await;
            open_streams.push(sse);
        }
        assert_eq!(fx.ctx.bus.size(), base + 5);

        let victim = open_streams.remove(2);
        drop(victim);
        assert_eq!(fx.ctx.bus.size(), base + 4);

        // The survivors still receive.
        fx.ctx.bus.publish(sample(Some("s1")));
        for sse in open_streams.iter_mut() {
            let got = sse.next().await;
            assert!(got.starts_with("event: message.delta\n"), "{got:?}");
        }

        drop(open_streams);
        assert_eq!(fx.ctx.bus.size(), base);
    }

    #[tokio::test]
    async fn teardown_is_idempotent_a_second_drop_path_cannot_double_release() {
        // In Rust the three TS triggers converge on one Drop, which runs once
        // by construction; what is left to pin is that a publish AFTER release
        // neither revives nor double-counts the subscription.
        let fx = testutil::fixture_bare();
        let base = fx.ctx.bus.size();
        let mut sse = open(&fx, no_heartbeat(), "/events").await;
        sse.next().await;
        assert_eq!(fx.ctx.bus.size(), base + 1);
        drop(sse);
        assert_eq!(fx.ctx.bus.size(), base);
        fx.ctx.bus.publish(sample(Some("s1")));
        assert_eq!(fx.ctx.bus.size(), base);
    }

    #[tokio::test]
    async fn dispatches_through_the_production_table() {
        use crate::app::{create_handler, CreateHandlerOptions};
        let fx = testutil::fixture_bare();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/events?sessionId=s1")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("content-type").unwrap(), "text/event-stream");

        let base = fx.ctx.bus.size() - 1; // this stream's own subscription
        let mut sse = Sse::new(res);
        assert_eq!(sse.next().await, CONNECTED_FRAME);
        let published = fx.ctx.bus.publish(sample(Some("s1")));
        assert_eq!(sse.next().await, frame(&published).unwrap());

        drop(sse);
        assert_eq!(fx.ctx.bus.size(), base, "the production handler must release too");
    }
}
