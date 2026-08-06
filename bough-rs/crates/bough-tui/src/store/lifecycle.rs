//! The message-lifecycle affordances (PORT_PLAN row 2.21): the take-back
//! window, the posted take-back (`unsend`), the ask-card settle verbs, and the
//! background-finish toast.
//!
//! WHERE THIS LIVES. The specs put the async verbs in `store/shell.rs` and
//! `take_back_target` in `forest.rs`; both are being ported by other rows in
//! parallel, so row 2.21's half sits here as its own module — plain functions
//! over `&Store` + `&Api`, which is exactly what an `impl Store` block would
//! be. Nothing else in the crate changes shape when they are folded together.
//!
//! THE INVARIANT THIS HOLDS: **the gesture is decided as data.** Whether
//! Escape pops a queued message, asks the server to forget a posted one, or
//! does nothing at all is [`take_back_target`] — a pure function, so the rule
//! is tested without a renderer or a socket, and the verbs below are only I/O.
//!
//! SECOND — **inside the window the take-back outranks the stop.** The keymap
//! (`keys.rs`) resolves Escape to `MessageUnsend` while `just_sent` holds and
//! to `TurnInterrupt` after it, and it may: unsend stops the turn on the way
//! out. Nobody takes a message back and still wants to pay for the answer.
//!
//! THIRD — **a refusal is an answer.** The server refuses to delete anything
//! but a session's own last user message (`history/ops/unsend.rs`); its
//! sentence is surfaced as a notice, because Escape that appears to do nothing
//! is worse than Escape that says why.

use crate::api::{Api, ApiFailure};
use crate::forest::{take_back_target, TakeBack};
use crate::keys::UNSEND_MS;

use super::selectors::{current_ask, is_busy};
use super::shell::Store;
use super::state::{BackgroundToast, StoreAction, TuiState};

// ---------------------------------------------------------------------------
// The take-back window
// ---------------------------------------------------------------------------

/// Is a send still inside the take-back window? Read at the KEYSTROKE rather
/// than held in state: the window expires on the clock, and a flag set at send
/// time would have to be cleared by a timer that can be missed.
pub fn just_sent(state: &TuiState, now: i64) -> bool {
    state.last_send_at.is_some_and(|at| now - at < UNSEND_MS)
}

/// What a completed take-back hands back to the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakenBack {
    /// The retracted text. The composer takes it with the cursor at the end.
    pub text: String,
    /// The session to re-read authoritatively, once, after the delete — the
    /// thread is now shorter and usage/changes moved with the stopped turn.
    /// `None` for the queued half, which never reached a server. Failure of
    /// that re-read is silent: the local drop already reflects what the server
    /// did, and the next resync repairs the rest.
    pub resync: Option<String>,
}

/// The whole gesture, decided then performed: `message.unsend`.
///
/// Returns `None` when there was nothing to take back (the window was armed by
/// a send whose message has not reached the thread yet — doing nothing beats
/// falling through to a stop the user did not ask for) or when the server
/// refused (it has said why, on the notice row).
pub async fn take_back(store: &Store, api: &Api) -> Option<TakenBack> {
    let (session_id, target, busy) = store.with_state(|s| {
        (s.current_id.clone(), take_back_target(&s.queued, &s.thread), is_busy(s))
    });
    match target {
        TakeBack::Queued => {
            let text = store.take_back_queued()?;
            Some(TakenBack { text, resync: None })
        }
        TakeBack::None => None,
        TakeBack::Sent { at_message_id, .. } => {
            let id = session_id?;
            // ONE call, not an interrupt raced against a delete: the route
            // stops the turn and drops the rows in that order, so the runner
            // cannot be mid-write on a message that is going away.
            let result = match api.unsend(&id, &at_message_id).await {
                Ok(result) => result,
                Err(error) => {
                    store.fail(&refusal(&error));
                    return None;
                }
            };
            // The text comes back from the SERVER rather than from `target`,
            // because the server is what decided the take-back was legal.
            store.dispatch(StoreAction::ThreadDropped {
                session_id: id.clone(),
                ids: result.removed,
            });
            store.notify(take_back_notice(busy));
            Some(TakenBack { text: result.text, resync: Some(id) })
        }
    }
}

/// Said in the past tense, and it names the turn only when there was one.
pub fn take_back_notice(busy: bool) -> &'static str {
    if busy {
        "took that back — the turn was stopped and the message is gone from this conversation"
    } else {
        "took that back — the message is gone from this conversation"
    }
}

fn refusal(error: &ApiFailure) -> String {
    error.to_string()
}

// ---------------------------------------------------------------------------
// The ask card's answers
// ---------------------------------------------------------------------------

/// Answer the hold the card is showing.
///
/// `descendants` is the same delegate list the CARD was rendered from
/// (`current_ask`): a subagent's hold is shown here and must therefore be
/// answerable here — filtering it out would park the delegate until its turn
/// timed out.
pub async fn answer_ask(store: &Store, api: &Api, descendants: &[&str], answer: &str) {
    settle_ask(store, api, descendants, Some(answer)).await
}

/// Decline it. The program's `ask()` rejects catchably with
/// `user declined to answer:` — a refusal is a real answer.
pub async fn decline_ask(store: &Store, api: &Api, descendants: &[&str]) {
    settle_ask(store, api, descendants, None).await
}

/// OPTIMISTIC, then reconciled. The hold leaves the card immediately so the
/// next one surfaces without a round-trip; if the settle is refused — already
/// answered, expired, or the turn was interrupted, all 409 — the holds are
/// re-read, because they are memory-only server-side and the server is the
/// only place that knows.
async fn settle_ask(store: &Store, api: &Api, descendants: &[&str], answer: Option<&str>) {
    let Some((session_id, id)) =
        store.with_state(|s| current_ask(s, descendants).map(|q| (q.session_id.clone(), q.id.clone())))
    else {
        return;
    };
    store.dispatch(StoreAction::AskSettled { id: id.clone() });
    let sent = match answer {
        Some(text) => api.answer_question(&session_id, &id, text).await.map(|_| ()),
        None => api.decline_question(&session_id, &id).await.map(|_| ()),
    };
    if sent.is_err() {
        match api.list_questions(None).await {
            Ok(questions) => {
                store.dispatch(StoreAction::Questions { questions });
            }
            Err(error) => store.fail(&error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// The background-finish toast
// ---------------------------------------------------------------------------

/// `✓ {title} finished — ^t opens the tree`.
///
/// `^t`, NOT `^s`: `^s` is the tree's alias and is guarded on an empty draft,
/// and this toast is loudest exactly where a draft exists — it appeared over a
/// fresh FORK, whose composer is prefilled by design, so the key it named could
/// not fire.
pub fn background_toast(done: &BackgroundToast) -> String {
    format!("✓ {} finished — ^t opens the tree", done.title)
}

/// The desktop notification's body — shorter, because it is read out of context.
pub fn background_desktop_body(done: &BackgroundToast) -> String {
    format!("{} finished", done.title)
}

/// Watches `background.seq` so the same news is announced exactly once.
///
/// `seq` rather than the session id is the dependency: the same session
/// finishing twice is two pieces of news, and seq 0 (the initial state) is
/// none at all.
#[derive(Default, Debug)]
pub struct BackgroundWatch {
    seen: u64,
}

impl BackgroundWatch {
    /// The toast to raise for this state — the notice row's line and the
    /// desktop notification's body — or None. Call after every dispatch; it is
    /// idempotent between changes.
    pub fn poll(&mut self, state: &TuiState) -> Option<(String, String)> {
        let done = state.background.as_ref()?;
        if done.seq == 0 || done.seq == self.seen {
            return None;
        }
        self.seen = done.seq;
        Some((background_toast(done), background_desktop_body(done)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiOptions, FetchFn, FetchRequest, HttpResponse};
    use crate::keys::{lookup, Command, KeyContext};
    use crate::store::state::{SessionRow, SessionSnapshot};
    use bough_core::schema::parts::Message;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    const SESSION: &str = "sess-1";

    fn msg(id: &str, role: &str, text: &str) -> Message {
        serde_json::from_value(json!({
            "id": id, "sessionId": SESSION, "role": role,
            "parts": [{"type": "text", "text": text}],
            "pending": false, "createdAt": 1,
        }))
        .unwrap()
    }

    fn pending(id: &str, text: &str) -> Message {
        serde_json::from_value(json!({
            "id": id, "sessionId": SESSION, "role": "supervisor",
            "parts": [{"type": "text", "text": text}],
            "pending": true, "createdAt": 2,
        }))
        .unwrap()
    }

    /// A fake transport: records every request, answers from a script.
    fn scripted(
        responses: Vec<Result<HttpResponse, String>>,
    ) -> (Api, Arc<Mutex<Vec<FetchRequest>>>) {
        let seen: Arc<Mutex<Vec<FetchRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(responses));
        let seen2 = seen.clone();
        let fetch: FetchFn = Arc::new(move |req| {
            seen2.lock().unwrap().push(req);
            let next = queue.lock().unwrap().remove(0);
            Box::pin(async move { next })
        });
        let api = Api::new(ApiOptions {
            base: Some("http://127.0.0.1:4321".into()),
            fetch_fn: Some(fetch),
        });
        (api, seen)
    }

    fn ok(body: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse { status: 200, body: body.into() })
    }

    fn open(store: &Store) {
        store.dispatch(StoreAction::Open { session_id: Some(SESSION.into()) });
    }

    fn seed_thread(store: &Store, thread: Vec<Message>) {
        store.dispatch(StoreAction::Snapshot {
            at: 0,
            snapshot: SessionSnapshot {
                session: serde_json::from_value(json!({
                    "id": SESSION, "title": "s", "kind": "root", "createdAt": 1, "parentId": null,
                }))
                .unwrap(),
                thread,
                usage: serde_json::from_value(json!({
                    "inputTokens": 0, "outputTokens": 0, "reasoningTokens": 0,
                    "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": 0.0,
                    "tree": { "inputTokens": 0, "outputTokens": 0, "reasoningTokens": 0,
                              "cacheReadTokens": 0, "cacheWriteTokens": 0, "costUsd": 0.0 },
                }))
                .unwrap(),
                effective_model: None,
                context_limit: None,
                primed_tags: None,
                project_rules: None,
            },
        });
    }

    // ---- the window ------------------------------------------------------

    /// keys.test.ts: "esc inside the take-back window unsends; outside it, it
    /// stops the turn" — the ORDERING is the gate for this row, so it is
    /// asserted here against the window predicate the App actually reads.
    #[test]
    fn inside_the_window_the_take_back_outranks_the_stop() {
        let store = Store::new();
        open(&store);
        store.dispatch(StoreAction::Sent { at: 5_000 });

        let ctx = |now: i64| KeyContext {
            busy: true,
            just_sent: just_sent(&store.get_state(), now),
            ..Default::default()
        };
        // Armed, and a turn is running: the take-back wins — it stops the turn
        // anyway, so nothing is lost by preferring it.
        assert!(just_sent(&store.get_state(), 5_000 + UNSEND_MS - 1));
        assert_eq!(lookup(&ctx(5_000 + UNSEND_MS - 1), "esc"), Some(Command::MessageUnsend));
        // One millisecond past the window Escape is the stop again.
        assert!(!just_sent(&store.get_state(), 5_000 + UNSEND_MS));
        assert_eq!(lookup(&ctx(5_000 + UNSEND_MS), "esc"), Some(Command::TurnInterrupt));
        // Idle inside the window: still the take-back, with nothing to stop.
        let idle = KeyContext { just_sent: true, ..Default::default() };
        assert_eq!(lookup(&idle, "esc"), Some(Command::MessageUnsend));
    }

    /// store.test.ts: "sending arms the take-back window, and a session switch
    /// disarms it" — the window belongs to the conversation you sent INTO.
    #[test]
    fn a_session_switch_disarms_the_window() {
        let store = Store::new();
        open(&store);
        store.dispatch(StoreAction::Sent { at: 5_000 });
        assert!(just_sent(&store.get_state(), 5_100));
        store.dispatch(StoreAction::Open { session_id: Some("sess-2".into()) });
        assert!(!just_sent(&store.get_state(), 5_100), "the window does not travel");
    }

    // ---- what the gesture acts on ----------------------------------------

    /// forest.test.ts: "the take-back prefers a queued message, then the last
    /// user turn".
    #[test]
    fn the_take_back_prefers_a_queued_message_then_the_last_user_turn() {
        let thread = vec![
            msg("m1", "user", "first"),
            msg("m2", "supervisor", "an answer"),
            msg("m3", "user", "the typo"),
        ];
        assert_eq!(
            take_back_target(&["typed while busy".to_string()], &thread),
            TakeBack::Queued
        );
        assert_eq!(
            take_back_target(&[], &thread),
            TakeBack::Sent { at_message_id: "m3".into(), text: "the typo".into() }
        );
        // A reply already streaming under it does not hide the user turn.
        let mut streaming = thread.clone();
        streaming.push(msg("m4", "supervisor", "validating…"));
        assert_eq!(
            take_back_target(&[], &streaming),
            TakeBack::Sent { at_message_id: "m3".into(), text: "the typo".into() }
        );
        // Nothing of the user's in the thread: nothing to take back, and the
        // caller must NOT fall through to a stop.
        assert_eq!(take_back_target(&[], &[]), TakeBack::None);
        assert_eq!(
            take_back_target(&[], &[msg("m1", "supervisor", "hi")]),
            TakeBack::None
        );
    }

    #[test]
    fn the_text_that_comes_back_is_the_whole_message_newlines_and_all() {
        let multiline = msg("m1", "user", "line one\nline two");
        assert_eq!(
            take_back_target(&[], &[multiline]),
            TakeBack::Sent { at_message_id: "m1".into(), text: "line one\nline two".into() }
        );
    }

    // ---- the posted half --------------------------------------------------

    #[tokio::test]
    async fn a_posted_take_back_deletes_the_message_and_hands_the_text_back() {
        let (api, seen) = scripted(vec![ok(
            r#"{"sessionId":"sess-1","text":"the typo","removed":["m3","m4"],"interrupted":true}"#,
        )]);
        let store = Store::new();
        open(&store);
        let mut thread = vec![
            msg("m1", "user", "first"),
            msg("m2", "supervisor", "an answer"),
            msg("m3", "user", "the typo"),
        ];
        thread.push(pending("m4", "half an ans"));
        seed_thread(&store, thread);
        store.dispatch(StoreAction::Sent { at: 5_000 });

        let taken = take_back(&store, &api).await.expect("the server allowed it");
        assert_eq!(taken.text, "the typo");
        assert_eq!(taken.resync.as_deref(), Some(SESSION), "the thread is re-read once, after");

        let req = &seen.lock().unwrap()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "http://127.0.0.1:4321/sessions/sess-1/unsend");
        assert_eq!(req.body.as_deref(), Some(r#"{"atMessageId":"m3"}"#));

        let state = store.get_state();
        // The message AND its half-written answer are gone…
        assert_eq!(state.thread.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m1", "m2"]);
        // …the window is disarmed by the drop…
        assert_eq!(state.last_send_at, None);
        // …and the outcome is said, naming the turn that was stopped.
        assert_eq!(state.notice.as_deref(), Some(take_back_notice(true)));
    }

    #[tokio::test]
    async fn an_idle_take_back_does_not_claim_it_stopped_a_turn() {
        let (api, _seen) = scripted(vec![ok(
            r#"{"sessionId":"sess-1","text":"the typo","removed":["m1"],"interrupted":false}"#,
        )]);
        let store = Store::new();
        open(&store);
        seed_thread(&store, vec![msg("m1", "user", "the typo")]);
        take_back(&store, &api).await.unwrap();
        assert_eq!(store.get_state().notice.as_deref(), Some(take_back_notice(false)));
    }

    #[tokio::test]
    async fn a_refusal_is_the_servers_sentence_and_nothing_is_dropped() {
        // The server refuses anything but the session's own LAST user message.
        let (api, _seen) = scripted(vec![Ok(HttpResponse {
            status: 400,
            body: r#"{"error":"only the last message in this conversation can be taken back"}"#
                .into(),
        })]);
        let store = Store::new();
        open(&store);
        seed_thread(&store, vec![msg("m1", "user", "the typo")]);

        assert_eq!(take_back(&store, &api).await, None, "refused — the composer keeps its draft");
        let state = store.get_state();
        assert_eq!(state.thread.len(), 1, "nothing was dropped locally");
        assert_eq!(
            state.notice.as_deref(),
            Some("only the last message in this conversation can be taken back"),
            "Escape that appears to do nothing is worse than Escape that says why",
        );
    }

    #[tokio::test]
    async fn a_queued_take_back_never_reaches_the_server() {
        let (api, seen) = scripted(vec![]);
        let store = Store::new();
        open(&store);
        seed_thread(&store, vec![msg("m1", "user", "posted")]);
        store.dispatch(StoreAction::Queue { text: "second thoughts".into() });

        let taken = take_back(&store, &api).await.unwrap();
        assert_eq!(taken.text, "second thoughts");
        assert_eq!(taken.resync, None);
        assert!(seen.lock().unwrap().is_empty(), "nothing outside this client knew about it");
        assert!(store.get_state().queued.is_empty());
    }

    #[tokio::test]
    async fn with_nothing_to_take_back_the_gesture_is_a_no_op_not_a_stop() {
        let (api, seen) = scripted(vec![]);
        let store = Store::new();
        open(&store);
        store.dispatch(StoreAction::Sent { at: 5_000 });
        assert_eq!(take_back(&store, &api).await, None);
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(store.get_state().notice, None);
    }

    // ---- the ask card's answers ------------------------------------------

    fn hold(id: &str, session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "sessionId": session_id, "messageId": "m-1",
            "question": "prod or staging?", "options": ["prod", "staging"],
            "status": "pending", "ts": 1,
        })
    }

    fn seed_hold(store: &Store, id: &str, session_id: &str) {
        store.dispatch(StoreAction::Questions {
            questions: vec![serde_json::from_value(hold(id, session_id)).unwrap()],
        });
    }

    #[tokio::test]
    async fn an_answer_settles_the_hold_optimistically_and_posts_it() {
        let (api, seen) = scripted(vec![ok(r#"{"ok":true,"id":"q1","status":"answered"}"#)]);
        let store = Store::new();
        open(&store);
        seed_hold(&store, "q1", SESSION);

        answer_ask(&store, &api, &[], "prod").await;
        let req = &seen.lock().unwrap()[0];
        assert_eq!(req.url, "http://127.0.0.1:4321/sessions/sess-1/questions/q1");
        assert_eq!(req.body.as_deref(), Some(r#"{"answer":"prod"}"#));
        // The card is already free: the next hold surfaces without a round-trip.
        assert!(store.get_state().asks.is_empty());
    }

    #[tokio::test]
    async fn a_decline_is_a_real_answer_and_takes_the_same_route() {
        let (api, seen) = scripted(vec![ok(r#"{"ok":true,"id":"q1","status":"declined"}"#)]);
        let store = Store::new();
        open(&store);
        seed_hold(&store, "q1", SESSION);

        decline_ask(&store, &api, &[]).await;
        let req = &seen.lock().unwrap()[0];
        assert_eq!(req.url, "http://127.0.0.1:4321/sessions/sess-1/questions/q1");
        assert_eq!(req.body.as_deref(), Some(r#"{"decline":true}"#));
    }

    /// The settled race: the hold expired (or another client answered it, or
    /// the turn was interrupted) between the card being drawn and the key being
    /// pressed. The server answers 409; the holds are memory-only there, so the
    /// only repair is to re-read them.
    #[tokio::test]
    async fn a_hold_that_settled_under_the_card_is_re_read_not_guessed() {
        let (api, seen) = scripted(vec![
            Ok(HttpResponse {
                status: 409,
                body: r#"{"error":"question q1 is no longer waiting for an answer"}"#.into(),
            }),
            // The re-read: this session still has a LATER hold waiting.
            ok(&serde_json::json!([hold("q2", SESSION)]).to_string()),
        ]);
        let store = Store::new();
        open(&store);
        seed_hold(&store, "q1", SESSION);

        answer_ask(&store, &api, &[], "prod").await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the refusal is followed by a re-read");
        assert_eq!(seen[1].method, "GET");
        assert_eq!(seen[1].url, "http://127.0.0.1:4321/questions");
        // The server's truth wins over the optimistic removal, in both
        // directions: q1 is gone and q2 is now the card.
        let state = store.get_state();
        assert_eq!(state.asks.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), ["q2"]);
        assert_eq!(current_ask(&state, &[]).unwrap().id, "q2");
    }

    #[tokio::test]
    async fn a_delegates_hold_is_answerable_from_the_card_that_shows_it() {
        // The card shows a subagent's hold (`current_ask` walks lineage), so
        // the settle must accept the same list — filtering it out here would
        // park the delegate until its turn timed out.
        let (api, seen) = scripted(vec![ok(r#"{"ok":true,"id":"q1","status":"answered"}"#)]);
        let store = Store::new();
        open(&store);
        seed_hold(&store, "q1", "agent-1");
        assert!(current_ask(&store.get_state(), &[]).is_none());

        answer_ask(&store, &api, &["agent-1"], "prod").await;
        assert_eq!(
            seen.lock().unwrap()[0].url,
            "http://127.0.0.1:4321/sessions/agent-1/questions/q1",
            "settled through the session that holds it — the server refuses any other",
        );
    }

    #[tokio::test]
    async fn with_no_hold_showing_the_ask_keys_do_nothing() {
        let (api, seen) = scripted(vec![]);
        let store = Store::new();
        open(&store);
        decline_ask(&store, &api, &[]).await;
        assert!(seen.lock().unwrap().is_empty());
    }

    // ---- the background toast --------------------------------------------

    fn finished_elsewhere(store: &Store, session_id: &str, title: &str, seq: u64) {
        // A row that is BUSY, then a `message.finished` for it while another
        // conversation is open — the reducer's own rule for "this is news".
        let row: SessionRow = serde_json::from_value(json!({
            "id": session_id, "title": title, "kind": "root", "createdAt": 1,
            "parentId": null, "busy": true,
        }))
        .unwrap();
        store.dispatch(StoreAction::Sessions { sessions: vec![row] });
        store.dispatch(StoreAction::Event {
            event: bough_core::schema::events::BoughEvent {
                r#type: bough_core::schema::events::EventType::MessageFinished,
                session_id: Some(session_id.into()),
                seq,
                ts: seq as i64,
                data: json!({ "messageId": "m-1" }),
            },
        });
    }

    #[test]
    fn a_conversation_you_are_not_looking_at_finishing_is_announced_once() {
        let store = Store::new();
        open(&store);
        let mut watch = BackgroundWatch::default();
        assert!(watch.poll(&store.get_state()).is_none(), "nothing has finished yet");

        finished_elsewhere(&store, "sess-2", "the other thing", 1);
        let state = store.get_state();
        // `^t`, NOT `^s`: this toast is loudest where a draft exists, and `^s`
        // is guarded on an empty one.
        assert_eq!(
            watch.poll(&state),
            Some((
                "✓ the other thing finished — ^t opens the tree".to_string(),
                "the other thing finished".to_string(),
            ))
        );

        // Idempotent between changes — a repaint is not new news.
        assert!(watch.poll(&store.get_state()).is_none());

        // The same session finishing again IS two pieces of news.
        finished_elsewhere(&store, "sess-2", "the other thing", 2);
        assert!(watch.poll(&store.get_state()).is_some());
    }
}
