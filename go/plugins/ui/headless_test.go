package ui

import (
	"bytes"
	"encoding/json"
	"reflect"
	"strings"
	"testing"
)

// Headless stream contract: "[assistant]"/"[done]" on stdout,
// "[error]" on stderr, and any error makes the exit code 1.
func TestHeadlessErrorsToStderrAndExitCode(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr := hlOut, hlErr
	hlOut, hlErr = &out, &errb
	defer func() {
		hlOut, hlErr = oldOut, oldErr
		hlErrored.Store(false)
	}()
	hlErrored.Store(false)

	hlPending.Add(1)
	hlPrint(Event{Kind: "assistant", Text: "hi"})
	hlPrint(Event{Kind: "done"})
	if got := ExitCode(); got != 0 {
		t.Fatalf("clean turn: ExitCode = %d, want 0", got)
	}

	hlPending.Add(1)
	hlPrint(Event{Kind: "error", Text: "boom"})
	hlPrint(Event{Kind: "done"})

	if s := out.String(); s != "[assistant] hi\n[done] \n[done] \n" {
		t.Fatalf("stdout = %q", s)
	}
	if s := errb.String(); s != "[error] boom\n" {
		t.Fatalf("stderr = %q", s)
	}
	if got := ExitCode(); got != 1 {
		t.Fatalf("errored turn: ExitCode = %d, want 1", got)
	}
}

// A line that arrives between "[assistant]" and "[done]" steers the
// running turn: it prints as "[steer]", the turn's own done pays for
// it, and the pending count ends at zero — the drain neither returns
// early nor goes negative.
func TestHeadlessSteerMidTurnKeepsPendingBalanced(t *testing.T) {
	var out bytes.Buffer
	oldOut := hlOut
	hlOut = &out
	hlMu.Lock()
	oldSteer := hlSteer
	hlSteer = func(string) bool { return true } // a turn runs: the loop takes it
	hlMu.Unlock()
	defer func() {
		hlOut = oldOut
		hlMu.Lock()
		hlSteer = oldSteer
		hlMu.Unlock()
	}()

	hlPending.Store(1) // the turn hlSubmit("a") started
	hlPrint(Event{Kind: "assistant", Text: "reply to a"})
	if !hlSteerLine("b") {
		t.Fatal("mid-turn line should steer")
	}
	hlPrint(Event{Kind: "steer", Text: "b"})
	hlPrint(Event{Kind: "assistant", Text: "reply to b"})
	hlPrint(Event{Kind: "done"})
	if n := hlPending.Load(); n != 0 {
		t.Fatalf("hlPending = %d after the one done, want 0", n)
	}
	want := "[assistant] reply to a\n[steer] b\n[assistant] reply to b\n[done] \n"
	if s := out.String(); s != want {
		t.Fatalf("stdout = %q, want %q", s, want)
	}
}

// Under --json a delta is its own line, tagged by kind, and the
// finished reply still prints exactly once as a whole.
func TestHeadlessJSONStreamsDeltas(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr, oldJSON := hlOut, hlErr, HeadlessJSON
	hlOut, hlErr, HeadlessJSON = &out, &errb, true
	defer func() {
		hlOut, hlErr, HeadlessJSON = oldOut, oldErr, oldJSON
		hlErrored.Store(false)
	}()

	hlPending.Add(1)
	hlPrint(Event{Kind: "thinking-delta", Text: "hmm"})
	hlPrint(Event{Kind: "assistant-delta", Text: "hi "})
	hlPrint(Event{Kind: "assistant-delta", Text: "there"})
	hlPrint(Event{Kind: "assistant", Text: "hi there"})
	hlPrint(Event{Kind: "done"})

	var kinds, texts []string
	for _, line := range strings.Split(strings.TrimSpace(out.String()), "\n") {
		var obj struct{ Kind, Text string }
		if err := json.Unmarshal([]byte(line), &obj); err != nil {
			t.Fatalf("line %q: %v", line, err)
		}
		kinds = append(kinds, obj.Kind)
		texts = append(texts, obj.Text)
	}
	wantKinds := []string{"thinking-delta", "assistant-delta", "assistant-delta", "assistant", "done"}
	if !reflect.DeepEqual(kinds, wantKinds) {
		t.Fatalf("kinds = %v, want %v", kinds, wantKinds)
	}
	if texts[3] != "hi there" {
		t.Fatalf("assistant text = %q, want the whole reply", texts[3])
	}
	// Exactly one finished reply: a delta must not print as one too.
	n := 0
	for _, k := range kinds {
		if k == "assistant" {
			n++
		}
	}
	if n != 1 {
		t.Fatalf("%d [assistant] lines, want 1", n)
	}
}

// Plain headless (no --json) still drops every delta: that stream is
// read by humans and by the bench harness.
func TestHeadlessPlainDropsDeltas(t *testing.T) {
	var out bytes.Buffer
	oldOut, oldJSON := hlOut, HeadlessJSON
	hlOut, HeadlessJSON = &out, false
	defer func() { hlOut, HeadlessJSON = oldOut, oldJSON }()

	hlPending.Add(1)
	hlPrint(Event{Kind: "thinking-delta", Text: "hmm"})
	hlPrint(Event{Kind: "assistant-delta", Text: "hi "})
	hlPrint(Event{Kind: "assistant-delta", Text: "there"})
	hlPrint(Event{Kind: "assistant", Text: "hi there"})
	hlPrint(Event{Kind: "done"})

	want := "[assistant] hi there\n[done] \n"
	if s := out.String(); s != want {
		t.Fatalf("stdout = %q, want %q", s, want)
	}
}

// The small model's live activity label reaches `bough serve` as its own
// JSON line (the clearing "" included) and never shows in plain output.
func TestHeadlessActivityJSONOnly(t *testing.T) {
	var out bytes.Buffer
	oldOut, oldJSON := hlOut, HeadlessJSON
	defer func() { hlOut, HeadlessJSON = oldOut, oldJSON }()

	hlOut, HeadlessJSON = &out, true
	hlPrint(Event{Kind: "activity", Text: "running the test suite"})
	hlPrint(Event{Kind: "activity", Text: ""})
	want := `{"kind":"activity","text":"running the test suite"}` + "\n" + `{"kind":"activity","text":""}` + "\n"
	if s := out.String(); s != want {
		t.Fatalf("json stdout = %q, want %q", s, want)
	}

	out.Reset()
	HeadlessJSON = false
	hlPrint(Event{Kind: "activity", Text: "running the test suite"})
	if s := out.String(); s != "" {
		t.Fatalf("plain stdout = %q, want nothing", s)
	}
}
