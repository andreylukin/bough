//! The store's I/O shell (port of `createStore` in `src/tui/store.ts` — the
//! non-reducer half).
//!
//! What is here in wave 1: the SYNC core — state ownership, subscriber
//! bookkeeping (leak-free over subscribe/unsubscribe cycles), `dispatch` with
//! the two timer-arming signals derived from STATE TRANSITIONS, `notify`,
//! `record` (notice + permanent mark in ONE call — the seam), `dismiss_notice`,
//! `take_back_queued`, and the constants the timers use.
//!
//! What is NOT here yet: the async API verbs (`send`/`open`/`unsend`/
//! `stop_unit`/`resync`/…) — they are I/O against the `Api` client, which is
//! row 1.32 and still a stub. The app loop (row 1.39) wires the real timers off
//! [`TimerSignals`]; nothing in this file owns a runtime, so the reducer stays
//! single-threaded by construction (all inputs arrive as actions over one mpsc).

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use super::reduce::reduce;
use super::state::{initial_state, StoreAction, TuiState};

/// How long a notice holds its pinned row before it expires. One duration for
/// every notice, deliberately.
pub const NOTICE_TTL_MS: u64 = 10_000;

/// How often the running turn re-reads its spend. Live only while a turn runs.
pub const USAGE_POLL_MS: u64 = 3_000;

/// A turn is in flight (started and not yet ended) — what gates the usage poll.
pub fn turn_running(state: &TuiState) -> bool {
    state.turn.as_ref().is_some_and(|t| t.ended_at.is_none())
}

/// The two timer edges `dispatch` derives from a state transition. The app loop
/// arms/disarms its tokio timers off these — armed from the TRANSITION rather
/// than from call sites, so a path added later cannot forget.
#[derive(Clone, Debug, PartialEq)]
pub struct TimerSignals {
    /// `Some(notice)` — (re)arm the notice expiry for this text; `Some(None)`
    /// is impossible by construction: `None` means the notice did not change.
    pub notice_changed: Option<Option<String>>,
    /// `Some(true)` — start the usage poll; `Some(false)` — stop it. `None` —
    /// no edge.
    pub usage_poll: Option<bool>,
}

type Listener = Rc<dyn Fn(&TuiState)>;

struct Inner {
    state: TuiState,
    listeners: Vec<(u64, Listener)>,
    next_id: u64,
    now: Rc<dyn Fn() -> i64>,
}

/// The store: single-threaded (the TUI runs one event loop task), so plain
/// `Rc<RefCell<…>>` — no locks, which is the property the TS suite pins.
pub struct Store {
    inner: Rc<RefCell<Inner>>,
}

impl Default for Store {
    fn default() -> Self {
        Store::new()
    }
}

impl Store {
    pub fn new() -> Store {
        Store::with_clock(Rc::new(|| 0))
    }

    /// The injected clock: the mark timestamps read this and nothing else does.
    /// Tests never sleep.
    pub fn with_clock(now: Rc<dyn Fn() -> i64>) -> Store {
        Store {
            inner: Rc::new(RefCell::new(Inner {
                state: initial_state(),
                listeners: Vec::new(),
                next_id: 1,
                now,
            })),
        }
    }

    pub fn get_state(&self) -> TuiState {
        self.inner.borrow().state.clone()
    }

    /// Read the state without cloning.
    pub fn with_state<R>(&self, f: impl FnOnce(&TuiState) -> R) -> R {
        f(&self.inner.borrow().state)
    }

    /// Subscribe; returns the id to pass to [`Store::unsubscribe`]. Called
    /// after every state change, never per event.
    pub fn subscribe(&self, listener: impl Fn(&TuiState) + 'static) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.listeners.push((id, Rc::new(listener)));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.inner.borrow_mut().listeners.retain(|(i, _)| *i != id);
    }

    #[cfg(test)]
    fn listener_count(&self) -> usize {
        self.inner.borrow().listeners.len()
    }

    /// Reduce and notify. Returns the timer edges this transition produced so
    /// the app loop can arm/disarm without watching state itself.
    pub fn dispatch(&self, action: StoreAction) -> TimerSignals {
        let (previous_notice, previous_running, next, listeners) = {
            let mut inner = self.inner.borrow_mut();
            let previous = inner.state.clone();
            let next = reduce(previous.clone(), action);
            if next == previous {
                return TimerSignals {
                    notice_changed: None,
                    usage_poll: None,
                };
            }
            inner.state = next.clone();
            let listeners: Vec<Listener> =
                inner.listeners.iter().map(|(_, l)| Rc::clone(l)).collect();
            let was_running = turn_running(&previous);
            (previous.notice, was_running, next, listeners)
        };
        let signals = TimerSignals {
            notice_changed: if next.notice != previous_notice {
                Some(next.notice.clone())
            } else {
                None
            },
            usage_poll: if turn_running(&next) != previous_running {
                Some(turn_running(&next))
            } else {
                None
            },
        };
        for listener in listeners {
            // One wedged renderer must not stop the others from being told.
            let _ = catch_unwind(AssertUnwindSafe(|| listener(&next)));
        }
        signals
    }

    /// Report a failure to the user instead of throwing into a render.
    pub fn fail(&self, error: &str) {
        self.dispatch(StoreAction::Notice {
            notice: Some(error.to_string()),
        });
    }

    /// A transient aside. Expires — see [`NOTICE_TTL_MS`].
    pub fn notify(&self, message: &str) {
        self.dispatch(StoreAction::Notice {
            notice: Some(message.to_string()),
        });
    }

    /// A destructive outcome: said now AND written into the transcript for
    /// good. THE SEAM — both halves in one call, so no future call site can do
    /// the reasonable half and drop the other. A failed kill is a notice and
    /// NO mark (nothing was destroyed) — callers route that through
    /// [`Store::fail`] instead.
    pub fn record(&self, message: &str) {
        self.dispatch(StoreAction::Notice {
            notice: Some(message.to_string()),
        });
        let (id, at) = {
            let inner = self.inner.borrow();
            (inner.state.current_id.clone(), (inner.now)())
        };
        if let Some(id) = id {
            self.dispatch(StoreAction::Mark {
                session_id: id,
                at,
                text: message.to_string(),
            });
        }
    }

    pub fn dismiss_notice(&self) {
        self.dispatch(StoreAction::Notice { notice: None });
    }

    /// Focus nothing, so the next message starts a fresh root conversation.
    pub fn new_conversation(&self) {
        self.dispatch(StoreAction::Open { session_id: None });
    }

    /// Take the most recently QUEUED message back, returning its text for the
    /// composer — None when nothing is queued. The easy half of the take-back
    /// gesture: nothing was ever posted, so the retraction is a local pop.
    pub fn take_back_queued(&self) -> Option<String> {
        let text = self.inner.borrow().state.queued.last().cloned()?;
        self.dispatch(StoreAction::QueuePop);
        Some(text)
    }
}

/// Ids the queued drain should post, in order — the pure half of `drainQueue`.
/// The shell empties the queue FIRST (`queue.drained`), then posts each; the
/// caller (app loop) owns the actual I/O.
pub fn drainable(state: &TuiState) -> Option<(String, Vec<String>)> {
    let id = state.current_id.clone()?;
    if state.queued.is_empty() || super::selectors::is_busy(state) {
        return None;
    }
    Some((id, state.queued.clone()))
}

/// Which refreshes an incoming event obliges the shell to run (the pure half of
/// the `onEvent` wiring in `createStore`). The app loop maps these to API calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFollowups {
    /// `turn.finished` anywhere: the change set has no event of its own.
    pub refresh_changes: bool,
    /// `turn.finished` for the OPEN session: snapshot → (fallback usage) →
    /// `turn.settle` AFTER the refetch, never before.
    pub settle_open_turn: bool,
    /// `turn.finished` elsewhere: reload the rows so its cost is not left at zero.
    pub reload_rows: bool,
    pub refresh_jobs: bool,
    pub refresh_workflows: bool,
    /// `turn.finished` or `session.created`: every change to `next_run_at` has
    /// one of these two signals, so the rail's countdown needs no poll.
    pub refresh_schedules: bool,
    /// `message.finished`: drain the local queue into a fresh turn.
    pub drain_queue: bool,
}

pub fn followups_for(
    event_type: bough_core::schema::events::EventType,
    event_session: Option<&str>,
    current: Option<&str>,
) -> EventFollowups {
    use bough_core::schema::events::EventType as E;
    let turn_finished = event_type == E::TurnFinished;
    let mine = event_session.is_some() && event_session == current;
    EventFollowups {
        refresh_changes: turn_finished,
        settle_open_turn: turn_finished && mine,
        reload_rows: turn_finished && !mine,
        refresh_jobs: matches!(event_type, E::JobSpawned | E::JobExited),
        refresh_workflows: matches!(event_type, E::WorkflowUpdated | E::WorkflowAgent),
        refresh_schedules: matches!(event_type, E::TurnFinished | E::SessionCreated),
        drain_queue: event_type == E::MessageFinished,
    }
}

// ---------------------------------------------------------------------------
// Tests — the shell cases that need no API (store.test.ts + retention.test.ts)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::selectors::marks_for;
    use super::super::state::*;
    use super::*;
    use bough_core::schema::events::EventType;
    use std::cell::Cell;

    const SESSION: &str = "sess-1";
    const OTHER: &str = "sess-2";

    fn open(store: &Store, id: &str) {
        store.dispatch(StoreAction::Open {
            session_id: Some(id.to_string()),
        });
    }

    #[test]
    fn n_subscribe_unsubscribe_cycles_leave_no_listener_behind() {
        // `Store::new` performs no I/O, so the subscriber bookkeeping is
        // testable with nothing mounted and nothing on the network.
        let store = Store::new();
        for i in 0..50 {
            let calls = Rc::new(Cell::new(0));
            let c = Rc::clone(&calls);
            let id = store.subscribe(move |_| c.set(c.get() + 1));
            store.dispatch(StoreAction::Notice {
                notice: Some(format!("notice {i}")),
            });
            assert_eq!(
                calls.get(),
                1,
                "cycle {i}: the live subscriber must be told"
            );
            store.unsubscribe(id);
            store.dispatch(StoreAction::Notice { notice: None });
            assert_eq!(
                calls.get(),
                1,
                "cycle {i}: a released subscriber must never be told again"
            );
        }
        assert_eq!(store.listener_count(), 0);

        // A detached listener that would panic proves nothing still holds it.
        let id = store.subscribe(|_| panic!("this listener was released and must not run"));
        store.unsubscribe(id);
        store.dispatch(StoreAction::Notice {
            notice: Some("after".into()),
        });
    }

    #[test]
    fn a_panicking_listener_does_not_silence_the_rest() {
        let store = Store::new();
        let told = Rc::new(Cell::new(false));
        store.subscribe(|_| panic!("wedged renderer"));
        let t = Rc::clone(&told);
        store.subscribe(move |_| t.set(true));
        store.dispatch(StoreAction::Notice {
            notice: Some("x".into()),
        });
        assert!(told.get(), "the second listener must still be told");
    }

    #[test]
    fn a_destructive_outcome_outlives_the_notice_that_announced_it() {
        let store = Store::with_clock(Rc::new(|| 42));
        open(&store, SESSION);

        store.record("reverted README.md");
        assert_eq!(
            store.get_state().notice.as_deref(),
            Some("reverted README.md")
        );
        let state = store.get_state();
        let marks: Vec<(_, _)> = marks_for(&state, Some(SESSION))
            .iter()
            .map(|m| (m.kind, m.text.clone()))
            .collect();
        assert_eq!(
            marks,
            vec![(MarkKind::Destructive, "reverted README.md".to_string())]
        );

        // The notice expires. The mark does not.
        store.dismiss_notice();
        assert_eq!(store.get_state().notice, None);
        assert_eq!(marks_for(&store.get_state(), Some(SESSION)).len(), 1);

        // …and neither does looking somewhere else and coming back.
        open(&store, OTHER);
        assert!(marks_for(&store.get_state(), Some(OTHER)).is_empty());
        open(&store, SESSION);
        assert_eq!(marks_for(&store.get_state(), Some(SESSION)).len(), 1);
    }

    #[test]
    fn a_queued_message_can_be_taken_back_before_it_is_ever_posted() {
        let store = Store::new();
        open(&store, SESSION);
        store.dispatch(StoreAction::Queue {
            text: "first".into(),
        });
        store.dispatch(StoreAction::Queue {
            text: "second thoughts".into(),
        });
        assert_eq!(store.get_state().queued, vec!["first", "second thoughts"]);

        // The newest comes back — an undo of the last send, not a purge.
        assert_eq!(store.take_back_queued().as_deref(), Some("second thoughts"));
        assert_eq!(store.get_state().queued, vec!["first"]);
        assert_eq!(store.take_back_queued().as_deref(), Some("first"));
        assert!(store.get_state().queued.is_empty());
        assert_eq!(
            store.take_back_queued(),
            None,
            "with nothing queued it is a no-op"
        );
    }

    #[test]
    fn timer_signals_are_edges_of_state_transitions_not_call_sites() {
        let store = Store::new();
        // A notice set → the expiry arms with the text.
        let s = store.dispatch(StoreAction::Notice {
            notice: Some("hi".into()),
        });
        assert_eq!(s.notice_changed, Some(Some("hi".into())));
        // Same notice again → no state change → no edge.
        let s = store.dispatch(StoreAction::Notice {
            notice: Some("hi".into()),
        });
        assert_eq!(s.notice_changed, None);
        // Cleared → the edge carries None so the timer is disarmed.
        let s = store.dispatch(StoreAction::Notice { notice: None });
        assert_eq!(s.notice_changed, Some(None));

        // A turn starting arms the usage poll; the settle stops it.
        open(&store, SESSION);
        let started = StoreAction::Event {
            event: bough_core::schema::events::BoughEvent {
                r#type: EventType::MessageStarted,
                session_id: Some(SESSION.into()),
                seq: 1,
                ts: 1,
                data: serde_json::json!({
                    "id": "m-1", "sessionId": SESSION, "role": "supervisor",
                    "parts": [], "pending": true, "createdAt": 1,
                }),
            },
        };
        let s = store.dispatch(started);
        assert_eq!(s.usage_poll, Some(true));
        let finished = StoreAction::Event {
            event: bough_core::schema::events::BoughEvent {
                r#type: EventType::TurnFinished,
                session_id: Some(SESSION.into()),
                seq: 2,
                ts: 2,
                data: serde_json::json!({"turnId": "t", "sessionId": SESSION, "status": "done"}),
            },
        };
        let s = store.dispatch(finished);
        assert_eq!(s.usage_poll, Some(false));
    }

    #[test]
    fn followups_route_a_turn_finishing_elsewhere_to_a_row_reload() {
        // `turn.finished` patches busy/status and nothing else, so a session
        // that ran while you were not looking must have its rows re-read or its
        // cost stays at zero.
        let f = followups_for(EventType::TurnFinished, Some(OTHER), Some(SESSION));
        assert!(f.refresh_changes);
        assert!(f.reload_rows);
        assert!(!f.settle_open_turn);
        assert!(f.refresh_schedules);

        let mine = followups_for(EventType::TurnFinished, Some(SESSION), Some(SESSION));
        assert!(mine.settle_open_turn);
        assert!(!mine.reload_rows);

        let msg = followups_for(EventType::MessageFinished, Some(SESSION), Some(SESSION));
        assert!(msg.drain_queue);
        assert!(!msg.refresh_changes);

        let created = followups_for(EventType::SessionCreated, Some(OTHER), Some(SESSION));
        assert!(created.refresh_schedules);
    }

    #[test]
    fn drainable_only_into_its_own_idle_session() {
        let store = Store::new();
        assert_eq!(drainable(&store.get_state()), None, "nothing open");
        open(&store, SESSION);
        assert_eq!(drainable(&store.get_state()), None, "nothing queued");
        store.dispatch(StoreAction::Queue {
            text: "while busy".into(),
        });
        let state = store.get_state();
        assert_eq!(
            drainable(&state),
            Some((SESSION.to_string(), vec!["while busy".to_string()]))
        );
        // A switch clears the queue: a staged message belongs to the session it
        // was typed in.
        open(&store, OTHER);
        assert_eq!(drainable(&store.get_state()), None);
    }
}
