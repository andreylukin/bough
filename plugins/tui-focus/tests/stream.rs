//! WP-4 / §2.4 + P3-D12: the `llm/stream` tee and the length rule that makes the handover from
//! the live tail to the durable steps flicker-free.
//!
//! The tee is driven here through `apply_tee`, which is the whole decision — delegate untouched,
//! or wrap — against a plain stream, with no kernel and no adapter.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_plugin_agents::AgentId;
use bough_plugin_llm::{CallConfig, Chunk, LlmRequest, StreamCall, StreamSlot};
use bough_plugin_tui_focus::{apply_tee, trailing_text, LiveText};
use futures::StreamExt;
use parking_lot::Mutex;

fn call_with(chunks: Vec<Chunk>) -> StreamCall {
    let slot = StreamSlot::empty();
    slot.put(futures::stream::iter(chunks).boxed());
    StreamCall {
        request: Arc::new(LlmRequest {
            model: "haiku".into(),
            system: None,
            system_volatile: None,
            messages: vec![],
            tools: vec![],
            call: CallConfig {
                model: "haiku".into(),
                max_tokens: 64,
                effort: None,
                tool_choice_none: false,
                meta: BTreeMap::new(),
            },
        }),
        cancel: tokio_util::sync::CancellationToken::new(),
        stream: slot,
    }
}

fn deltas(words: &[&str]) -> Vec<Chunk> {
    words
        .iter()
        .map(|w| Chunk::TextDelta {
            text: (*w).to_string(),
        })
        .collect()
}

/// The point of the tee: what has streamed is on screen BEFORE `flush_text` has written a single
/// `thought/text` step. With no durable text at all, the tail is what is rendered.
#[tokio::test]
async fn live_deltas_render_before_the_durable_step_lands() {
    let live = Arc::new(Mutex::new(LiveText::default()));
    let redraws = Arc::new(AtomicUsize::new(0));
    let r = redraws.clone();
    let sol = AgentId::new("sol");

    let call = call_with(deltas(&["Hel", "lo ", "world"]));
    assert!(
        apply_tee(
            &call,
            Some(sol.clone()),
            Some(&sol),
            live.clone(),
            Arc::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        ),
        "a stream with an ambient initiator is teed"
    );

    let mut stream = call.stream.take().expect("the tee put a stream back");
    // One chunk consumed: the tail already holds it, and the durable ledger holds nothing.
    let first = stream.next().await.expect("a chunk");
    assert_eq!(
        first,
        Chunk::TextDelta { text: "Hel".into() },
        "the tee is an OBSERVER: the chunk passes through byte-identical"
    );
    assert_eq!(live.lock().text, "Hel");
    assert_eq!(live.lock().agent, Some(sol.clone()));
    assert_eq!(trailing_text("", &live.lock().text), "Hel");
    assert_eq!(
        redraws.load(Ordering::SeqCst),
        1,
        "each delta asks for a frame"
    );

    while stream.next().await.is_some() {}
    assert_eq!(live.lock().text, "Hello world");
    assert_eq!(redraws.load(Ordering::SeqCst), 3);
}

/// P3-D12: the flushes concatenate to a PREFIX of what streamed, so the length rule alone decides
/// which half is drawn — and neither half is ever drawn on top of the other.
#[test]
fn the_durable_step_replaces_the_live_tail_without_flicker() {
    // Mid-stream: the tail is ahead of the last flush, so the tail is what is shown.
    assert_eq!(trailing_text("Hello", "Hello wor"), "Hello wor");
    // Exactly level: still the tail — the two are the same bytes, and picking one is what keeps
    // the handover from flickering between two equal strings.
    assert_eq!(trailing_text("Hello wor", "Hello wor"), "Hello wor");
    // The wake ended and the tail was cleared: the durable text is the whole truth.
    assert_eq!(trailing_text("Hello world", ""), "Hello world");
    // And a tail that somehow lags the ledger never un-renders text that is already durable.
    assert_eq!(trailing_text("Hello world", "Hel"), "Hello world");
    assert_eq!(trailing_text("", ""), "");
}

/// §2's initiator is AMBIENT ATTRIBUTION. Outside a wake there is no initiator, and the tee has
/// nobody to attribute the text to — so it replaces nothing and short-circuits nothing.
#[tokio::test]
async fn a_stream_with_no_initiator_is_delegated_untouched() {
    let live = Arc::new(Mutex::new(LiveText::default()));
    let call = call_with(deltas(&["a", "b"]));

    assert!(
        !apply_tee(&call, None, None, live.clone(), Arc::new(|| {})),
        "no ambient initiator ⇒ no tee"
    );

    // The slot still carries the ORIGINAL stream, whole, in order.
    let mut stream = call.stream.take().expect("the stream is still there");
    let mut got = Vec::new();
    while let Some(c) = stream.next().await {
        got.push(c);
    }
    assert_eq!(got, deltas(&["a", "b"]));
    assert_eq!(*live.lock(), LiveText::default(), "nothing was captured");

    // A stream initiated by an agent this pane is NOT showing is equally untouched: the pane draws
    // one trajectory, and another agent's text is not its tail.
    let call = call_with(deltas(&["x"]));
    assert!(!apply_tee(
        &call,
        Some(AgentId::new("terra")),
        Some(&AgentId::new("sol")),
        live.clone(),
        Arc::new(|| {}),
    ));
    assert_eq!(*live.lock(), LiveText::default());
}
