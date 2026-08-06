//! In-process fan-out to the SSE subscribers (port of `src/bus.ts`).
//!
//! "One bad subscriber cannot silence the others. A listener that throws is
//! caught, reported, and stepped over; fan-out continues down the set."
//!
//! "The bus is display transport, never storage. It holds no history and
//! replays nothing. `seq` is a process-monotonic counter that resets on
//! restart, so it is a dedupe key and not a resume cursor — a reconnecting
//! client re-fetches the session and reconciles by message id. Persist first,
//! then publish."
//!
//! There is no module-level singleton; the bus travels in `AppCtx`.
//! Hand-rolled synchronous fan-out, NOT `tokio::broadcast` (async-delivered,
//! drops on lag, and cannot express "a listener unsubscribed mid-fan-out does
//! not receive the in-flight event" — all three are test-pinned).

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::schema::events::{BoughEvent, EventInput};
use crate::types::Clock;

pub type Listener = Arc<dyn Fn(&BoughEvent) + Send + Sync>;
pub type ListenerErrorHook = Arc<dyn Fn(&str, &BoughEvent) + Send + Sync>;

pub struct Bus {
    seq: Mutex<u64>,
    /// Keyed slab: iteration re-checks membership per call (live-set
    /// semantics — an unsubscribe mid-fan-out skips delivery).
    listeners: Mutex<HashMap<u64, Listener>>,
    next_id: Mutex<u64>,
    now: Clock,
    on_listener_error: ListenerErrorHook,
}

impl Bus {
    pub fn new(now: Clock) -> Self {
        Self::with_error_hook(
            now,
            Arc::new(|err, _event| tracing::error!("bus listener threw: {err}")),
        )
    }

    pub fn with_error_hook(now: Clock, on_listener_error: ListenerErrorHook) -> Self {
        Bus {
            seq: Mutex::new(0),
            listeners: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
            now,
            on_listener_error,
        }
    }

    /// Stamps `seq` (starts at 1) and `ts`, delivers synchronously to every
    /// subscriber in subscription order, returns the stamped event. Never
    /// fails for a listener's reason.
    pub fn publish(&self, event: EventInput) -> BoughEvent {
        let seq = {
            let mut s = self.seq.lock().unwrap();
            *s += 1;
            *s
        };
        let stamped = BoughEvent {
            r#type: event.r#type,
            session_id: event.session_id,
            seq,
            ts: (self.now)(),
            data: event.data,
        };
        // Snapshot the ids in insertion order, then re-check membership per
        // call: an id unsubscribed mid-fan-out must not receive this event.
        let mut ids: Vec<u64> = self.listeners.lock().unwrap().keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let listener = self.listeners.lock().unwrap().get(&id).cloned();
            let Some(listener) = listener else { continue };
            let result = catch_unwind(AssertUnwindSafe(|| listener(&stamped)));
            if let Err(panic) = result {
                let msg = panic_message(&panic);
                // The reporter itself must not break fan-out either.
                let hook = self.on_listener_error.clone();
                let _ = catch_unwind(AssertUnwindSafe(|| hook(&msg, &stamped)));
            }
        }
        stamped
    }

    /// Returns the subscription id; pass it to [`Bus::unsubscribe`].
    /// Idempotent unsubscribe, safe to call from inside a listener.
    pub fn subscribe(&self, listener: Listener) -> u64 {
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n
        };
        self.listeners.lock().unwrap().insert(id, listener);
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.listeners.lock().unwrap().remove(&id);
    }

    /// Live subscriber count — the leak check in the SSE tests reads it.
    pub fn size(&self) -> usize {
        self.listeners.lock().unwrap().len()
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::events::EventType;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    /// A minimal well-formed event. The bus does not inspect payloads.
    fn delta(text: &str) -> EventInput {
        EventInput {
            r#type: EventType::MessageDelta,
            session_id: Some("s1".into()),
            data: json!({"messageId": "m1", "delta": text}),
        }
    }

    /// A bus whose listener errors are collected instead of logged, so runs
    /// stay quiet and the isolation itself can be asserted.
    fn quiet_bus() -> (Arc<Bus>, Arc<Mutex<Vec<String>>>) {
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let e = errors.clone();
        let bus = Arc::new(Bus::with_error_hook(
            Arc::new(|| 42),
            Arc::new(move |err, _event| e.lock().unwrap().push(err.to_string())),
        ));
        (bus, errors)
    }

    // ---- invariant: a throwing listener must not break fan-out --------------

    #[test]
    fn a_throwing_listener_does_not_prevent_later_listeners() {
        // Checked with throwers *before* healthy listeners — the position that
        // actually breaks — rather than last, where a broken implementation
        // would pass by accident.
        let (b, errors) = quiet_bus();
        let seen = Arc::new(Mutex::new(Vec::<&str>::new()));
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("first")));
        b.subscribe(Arc::new(|_| panic!("this SSE connection is gone")));
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("third")));
        b.subscribe(Arc::new(|_| panic!("so is this one")));
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("fifth")));

        b.publish(delta("x"));

        assert_eq!(*seen.lock().unwrap(), vec!["first", "third", "fifth"]);
        let errs = errors.lock().unwrap();
        assert_eq!(errs.len(), 2, "both throws were reported, not swallowed silently");
        assert_eq!(errs[0], "this SSE connection is gone");
    }

    #[test]
    fn publish_never_fails_for_a_listeners_reason() {
        let (b, _) = quiet_bus();
        b.subscribe(Arc::new(|_| panic!("boom")));
        // Callers persist and then publish; an emit path that could fail would
        // need an error branch at every call site.
        assert_eq!(b.publish(delta("x")).seq, 1);
    }

    #[test]
    fn a_reporter_that_panics_is_still_isolated() {
        let seen = Arc::new(Mutex::new(Vec::<&str>::new()));
        let b = Bus::with_error_hook(
            Arc::new(|| 0),
            Arc::new(|_, _| panic!("the reporter is broken too")),
        );
        b.subscribe(Arc::new(|_| panic!("boom")));
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("survivor")));

        b.publish(delta("x"));
        assert_eq!(*seen.lock().unwrap(), vec!["survivor"]);
    }

    #[test]
    fn the_default_reporter_does_not_break_fanout() {
        // `Bus::new` wires the tracing logger; a panicking listener must still
        // be stepped over.
        let b = Bus::new(Arc::new(|| 0));
        let seen = Arc::new(Mutex::new(Vec::<&str>::new()));
        b.subscribe(Arc::new(|_| panic!("boom")));
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("survivor")));
        b.publish(delta("x"));
        assert_eq!(*seen.lock().unwrap(), vec!["survivor"]);
    }

    // ---- seq is monotonic ---------------------------------------------------

    #[test]
    fn seq_starts_at_one_and_increments_by_exactly_one() {
        let (b, _) = quiet_bus();
        let seqs = Arc::new(Mutex::new(Vec::<u64>::new()));
        let s = seqs.clone();
        b.subscribe(Arc::new(move |e| s.lock().unwrap().push(e.seq)));

        for i in 0..50 {
            b.publish(delta(&format!("d{i}")));
        }

        let seqs = seqs.lock().unwrap();
        assert_eq!(*seqs, (1..=50).collect::<Vec<u64>>());
    }

    #[test]
    fn seq_advances_with_zero_listeners_and_when_every_listener_throws() {
        let (b, errors) = quiet_bus();
        // Nothing subscribed: the counter is a property of the bus, not of delivery.
        assert_eq!(b.publish(delta("a")).seq, 1);
        assert_eq!(b.publish(delta("b")).seq, 2);
        b.subscribe(Arc::new(|_| panic!("x")));
        assert_eq!(b.publish(delta("c")).seq, 3);
        assert_eq!(b.publish(delta("d")).seq, 4);
        assert_eq!(errors.lock().unwrap().len(), 2);
    }

    #[test]
    fn every_subscriber_sees_the_same_seq_and_no_repeats() {
        let (b, _) = quiet_bus();
        let a = Arc::new(Mutex::new(Vec::<u64>::new()));
        let bb = Arc::new(Mutex::new(Vec::<u64>::new()));
        let s = a.clone();
        b.subscribe(Arc::new(move |e| s.lock().unwrap().push(e.seq)));
        let s = bb.clone();
        b.subscribe(Arc::new(move |e| s.lock().unwrap().push(e.seq)));

        b.publish(delta("1"));
        b.publish(delta("2"));
        b.publish(delta("3"));

        let a = a.lock().unwrap();
        assert_eq!(*a, vec![1, 2, 3]);
        assert_eq!(*bb.lock().unwrap(), *a, "one seq per event, not one per delivery");
    }

    #[test]
    fn seq_is_per_instance_no_shared_global_counter() {
        let (one, _) = quiet_bus();
        let (two, _) = quiet_bus();
        assert_eq!(one.publish(delta("a")).seq, 1);
        assert_eq!(one.publish(delta("b")).seq, 2);
        assert_eq!(two.publish(delta("a")).seq, 1);
    }

    // ---- stamping -----------------------------------------------------------

    #[test]
    fn publish_stamps_ts_from_the_injected_clock_and_returns_the_stamped_event() {
        let t = Arc::new(AtomicI64::new(1000));
        let clock = t.clone();
        let b = Bus::with_error_hook(
            Arc::new(move || clock.fetch_add(5, Ordering::SeqCst) + 5),
            Arc::new(|_, _| {}),
        );
        let received = Arc::new(Mutex::new(Vec::<BoughEvent>::new()));
        let r = received.clone();
        b.subscribe(Arc::new(move |e| r.lock().unwrap().push(e.clone())));

        let returned = b.publish(delta("hello"));

        assert_eq!(returned.ts, 1005);
        assert_eq!(returned.seq, 1);
        assert_eq!(returned.r#type, EventType::MessageDelta);
        assert_eq!(returned.session_id.as_deref(), Some("s1"));
        assert_eq!(returned.data, json!({"messageId": "m1", "delta": "hello"}));
        {
            // Scoped: the guard must drop before the next publish, or the
            // listener's own lock would deadlock against it.
            let received = received.lock().unwrap();
            assert_eq!(received.len(), 1);
            assert_eq!(received[0], returned, "listeners get the event publish returns");
        }

        assert_eq!(b.publish(delta("again")).ts, 1010);
    }

    #[test]
    fn delivery_is_synchronous_no_task_hop() {
        let (b, _) = quiet_bus();
        let delivered = Arc::new(AtomicUsize::new(0));
        let d = delivered.clone();
        b.subscribe(Arc::new(move |_| {
            d.fetch_add(1, Ordering::SeqCst);
        }));
        b.publish(delta("x"));
        // If this were deferred, two emits could reach a client out of seq order.
        assert_eq!(delivered.load(Ordering::SeqCst), 1, "listener ran before publish returned");
    }

    // ---- subscribe / unsubscribe --------------------------------------------

    #[test]
    fn unsubscribe_detaches_and_size_tracks_live_listeners() {
        let (b, _) = quiet_bus();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        assert_eq!(b.size(), 0);

        let s = seen.clone();
        let off = b.subscribe(Arc::new(move |e| s.lock().unwrap().push(format!("a:{}", e.seq))));
        let s = seen.clone();
        b.subscribe(Arc::new(move |e| s.lock().unwrap().push(format!("b:{}", e.seq))));
        assert_eq!(b.size(), 2);

        b.publish(delta("1"));
        b.unsubscribe(off);
        assert_eq!(b.size(), 1);
        b.publish(delta("2"));

        assert_eq!(*seen.lock().unwrap(), vec!["a:1", "b:1", "b:2"]);
    }

    #[test]
    fn unsubscribe_idempotent_and_100_cycles_leak_nothing() {
        let (b, _) = quiet_bus();
        let id = b.subscribe(Arc::new(|_| {}));
        b.unsubscribe(id);
        b.unsubscribe(id);
        assert_eq!(b.size(), 0);

        for _ in 0..100 {
            let id = b.subscribe(Arc::new(|_| {}));
            b.unsubscribe(id);
        }
        assert_eq!(b.size(), 0, "the SSE endpoint's cancel path depends on this");
    }

    #[test]
    fn a_listener_may_unsubscribe_itself_mid_fanout() {
        let (b, _) = quiet_bus();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let own_id = Arc::new(Mutex::new(0u64));

        let s = seen.clone();
        let b2 = b.clone();
        let me = own_id.clone();
        let id = b.subscribe(Arc::new(move |e| {
            s.lock().unwrap().push(format!("once:{}", e.seq));
            b2.unsubscribe(*me.lock().unwrap());
        }));
        *own_id.lock().unwrap() = id;
        let s = seen.clone();
        b.subscribe(Arc::new(move |e| s.lock().unwrap().push(format!("always:{}", e.seq))));

        b.publish(delta("1"));
        b.publish(delta("2"));

        assert_eq!(*seen.lock().unwrap(), vec!["once:1", "always:1", "always:2"]);
        assert_eq!(b.size(), 1);
    }

    #[test]
    fn a_listener_unsubscribed_by_an_earlier_listener_does_not_receive() {
        // Iteration is over the live set: an unsubscribe is a closed
        // connection, so the safe direction is to skip it rather than deliver
        // and swallow the error.
        let (b, _) = quiet_bus();
        let seen = Arc::new(Mutex::new(Vec::<&str>::new()));
        let victim_id = Arc::new(Mutex::new(0u64));

        let s = seen.clone();
        let b2 = b.clone();
        let v = victim_id.clone();
        b.subscribe(Arc::new(move |_| {
            s.lock().unwrap().push("first");
            b2.unsubscribe(*v.lock().unwrap());
        }));
        let s = seen.clone();
        let id = b.subscribe(Arc::new(move |_| s.lock().unwrap().push("second")));
        *victim_id.lock().unwrap() = id;
        let s = seen.clone();
        b.subscribe(Arc::new(move |_| s.lock().unwrap().push("third")));

        b.publish(delta("1"));
        assert_eq!(*seen.lock().unwrap(), vec!["first", "third"]);
        assert_eq!(b.size(), 2);
    }
}
