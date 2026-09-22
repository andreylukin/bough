package ui

import (
	"bytes"
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

// Headless on the engine (go/docs/unreal-engine.md §10.4). These share
// the package's headless globals, like the rest of headless_test.go, so
// they do not run in parallel.

// Under --json a running call's live output and a stream reset carry
// the data a reader needs (the call id, the request seq); plain output
// drops both, like every other fragment.
func TestHeadlessJSONForwardsCallDelta(t *testing.T) {
	var out bytes.Buffer
	oldOut, oldJSON := hlOut, HeadlessJSON
	hlOut, HeadlessJSON = &out, true
	defer func() { hlOut, HeadlessJSON = oldOut, oldJSON }()

	hlPrint(Event{Kind: "call-delta", Text: "ok pkg\n", Data: map[string]any{"id": "toolu_1"}})
	hlPrint(Event{Kind: "delta-reset", Data: map[string]any{"seq": 3}})
	var got []map[string]any
	for _, l := range strings.Split(strings.TrimSpace(out.String()), "\n") {
		var obj map[string]any
		if err := json.Unmarshal([]byte(l), &obj); err != nil {
			t.Fatalf("line %q: %v", l, err)
		}
		got = append(got, obj)
	}
	if len(got) != 2 || got[0]["kind"] != "call-delta" || got[0]["id"] != "toolu_1" || got[0]["text"] != "ok pkg\n" {
		t.Fatalf("call-delta line = %v", got)
	}
	if got[1]["kind"] != "delta-reset" || got[1]["seq"] != 3.0 {
		t.Fatalf("delta-reset line = %v", got[1])
	}

	out.Reset()
	HeadlessJSON = false
	hlPrint(Event{Kind: "call-delta", Text: "ok pkg\n", Data: map[string]any{"id": "toolu_1"}})
	hlPrint(Event{Kind: "delta-reset", Data: map[string]any{"seq": 3}})
	if out.Len() != 0 {
		t.Fatalf("plain output printed fragments: %q", out.String())
	}
}

// A wake turn's done was never paid for by a stdin line: it must not
// take one off, or the drain ends while a typed line still runs.
func TestHeadlessWakeDoneKeepsPending(t *testing.T) {
	var out bytes.Buffer
	oldOut := hlOut
	hlOut = &out
	defer func() { hlOut = oldOut }()

	hlPending.Store(1)
	hlPrint(Event{Kind: "assistant", Text: "the build finished"})
	hlPrint(Event{Kind: "done", Data: map[string]any{"wake": true}})
	if n := hlPending.Load(); n != 1 {
		t.Fatalf("hlPending = %d after a wake done, want 1", n)
	}
	hlPrint(Event{Kind: "done"})
	if n := hlPending.Load(); n != 0 {
		t.Fatalf("hlPending = %d after the line's done, want 0", n)
	}
}

// A native call that failed is a call row, not an error: the model
// recovers from it inside the turn, and the run exits 0.
func TestHeadlessFailedCallThenSuccessExitsZero(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr := hlOut, hlErr
	hlOut, hlErr = &out, &errb
	defer func() {
		hlOut, hlErr = oldOut, oldErr
		hlErrored.Store(false)
	}()
	hlErrored.Store(false)

	hlPending.Store(1)
	hlPrint(Event{Kind: "call", Text: "go test ./...", Data: map[string]any{"id": "c1", "tool": "bash", "phase": "start"}})
	hlPrint(Event{Kind: "call", Text: "go test ./...", Data: map[string]any{"id": "c1", "tool": "bash", "exit": 1, "error": "exit status 1"}})
	hlPrint(Event{Kind: "call", Text: "go test ./...", Data: map[string]any{"id": "c2", "tool": "bash", "exit": 0}})
	hlPrint(Event{Kind: "assistant", Text: "fixed"})
	hlPrint(Event{Kind: "done"})
	if got := ExitCode(); got != 0 {
		t.Fatalf("ExitCode = %d, want 0 (stderr %q)", got, errb.String())
	}
	if errb.Len() != 0 {
		t.Fatalf("stderr = %q, want nothing", errb.String())
	}
}

// At EOF the engine's drain runs after the turns the lines started, and
// returns when the engine says it is idle.
func TestDrainEngineWaitsForDrain(t *testing.T) {
	release := make(chan struct{})
	ran := make(chan struct{})
	hlMu.Lock()
	old := hlDrain
	hlDrain = func() func(context.Context) error {
		return func(ctx context.Context) error {
			close(ran)
			select {
			case <-release:
				return nil
			case <-ctx.Done():
				return ctx.Err()
			}
		}
	}
	hlMu.Unlock()
	defer func() {
		hlMu.Lock()
		hlDrain = old
		hlMu.Unlock()
	}()

	finished := make(chan struct{})
	go func() { drainEngine(); close(finished) }()
	<-ran
	select {
	case <-finished:
		t.Fatal("drainEngine returned before the engine drained")
	case <-time.After(50 * time.Millisecond):
	}
	close(release)
	select {
	case <-finished:
	case <-time.After(5 * time.Second):
		t.Fatal("drainEngine did not return after the drain")
	}
}

// With no loop event for BOUGH_HEADLESS_IDLE the drain is given up: a
// hung call must not hold a one-shot run forever.
func TestDrainEngineIdleGivesUp(t *testing.T) {
	t.Setenv("BOUGH_HEADLESS_IDLE", "1")
	hlMu.Lock()
	old := hlDrain
	hlDrain = func() func(context.Context) error {
		return func(ctx context.Context) error { <-ctx.Done(); return ctx.Err() }
	}
	hlMu.Unlock()
	defer func() {
		hlMu.Lock()
		hlDrain = old
		hlMu.Unlock()
	}()
	finished := make(chan struct{})
	go func() { drainEngine(); close(finished) }()
	select {
	case <-finished:
	case <-time.After(10 * time.Second):
		t.Fatal("drainEngine never gave up")
	}
	// No drain key (the loop): nothing to wait for.
	hlMu.Lock()
	hlDrain = func() func(context.Context) error { return nil }
	hlMu.Unlock()
	drainEngine()
}
