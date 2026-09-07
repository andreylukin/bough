package cmux

import (
	"context"
	"errors"
	"os"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestDetect(t *testing.T) {
	env := func(m map[string]string) func(string) string { return func(k string) string { return m[k] } }
	noPath := func(string) (string, error) { return "", errors.New("not found") }
	noStat := func(string) (os.FileInfo, error) { return nil, os.ErrNotExist }
	if cli, _ := Detect(env(nil), noPath, noStat); cli != "" {
		t.Fatal("outside cmux: nothing")
	}
	if cli, ws := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), func(string) (string, error) { return "/usr/local/bin/cmux", nil }, noStat); cli != "/usr/local/bin/cmux" || ws != "workspace:2" {
		t.Fatalf("on PATH: %q %q", cli, ws)
	}
	if cli, _ := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), noPath, func(p string) (os.FileInfo, error) {
		if p == candidates[0] {
			return nil, nil
		}
		return nil, os.ErrNotExist
	}); cli != candidates[0] {
		t.Fatalf("bundled: %q", cli)
	}
	if cli, _ := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), noPath, noStat); cli != "" {
		t.Fatal("no CLI anywhere: nothing")
	}
}

// recorder is a Namer over a fake cmux that records the calls.
type recorder struct {
	*Namer
	mu    sync.Mutex
	calls []string
}

func newRecorder(prefix string) *recorder {
	r := &recorder{}
	r.Namer = newNamer("cmux", "workspace:2", prefix, func(_ context.Context, args ...string) error {
		r.mu.Lock()
		r.calls = append(r.calls, strings.Join(args, " "))
		r.mu.Unlock()
		return nil
	})
	return r
}

// settled waits for the queue to drain and returns the calls so far.
func (r *recorder) settled(t *testing.T) []string {
	t.Helper()
	done := make(chan struct{})
	r.enqueue(func() { close(done) })
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("queue never drained")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.calls...)
}

func TestNameOncePerTitle(t *testing.T) {
	r := newRecorder("bough · ")
	r.Name("Deploy the event log")
	r.Name("Deploy the event log")
	r.Name("  ")
	r.Name("Second title")
	calls := r.settled(t)
	if len(calls) != 2 {
		t.Fatalf("calls: %v", calls)
	}
	if calls[0] != "rename-workspace --workspace workspace:2 -- bough · Deploy the event log" {
		t.Fatalf("first call: %v", calls[0])
	}
}

// A turn's first content flips the pill to working and puts the prompt
// in the description; activity labels refine the pill; done ends it.
// Nothing is sent twice for the same text.
func TestEventDrivesPillAndDescription(t *testing.T) {
	r := newRecorder("")
	prompt := func() string { return "fix the flaky test\nmore detail" }
	r.Event("thinking-delta", "hm", prompt)
	r.Event("assistant-delta", "I will", prompt)
	r.Event("activity", "reading the test", prompt)
	r.Event("activity", "reading the test", prompt)
	r.Event("done", "", prompt)
	r.Event("activity", "late label", prompt) // after the turn: dropped
	calls := r.settled(t)
	want := []string{
		"set-status bough working --workspace workspace:2 --icon bolt.fill --color #ff9500 --priority 80",
		"workspace-action --workspace workspace:2 --action set-description --description fix the flaky test",
		"set-status bough reading the test --workspace workspace:2 --icon bolt.fill --color #ff9500 --priority 80",
		"set-status bough done · waiting for you --workspace workspace:2 --icon checkmark.circle.fill --color #34c759 --priority 80",
	}
	if strings.Join(calls, "\n") != strings.Join(want, "\n") {
		t.Fatalf("calls:\n%s\nwant:\n%s", strings.Join(calls, "\n"), strings.Join(want, "\n"))
	}
}

// A question flips the pill to asking; the answer's content brings it
// back to working. Esc shows stopped.
func TestEventAskAndCancel(t *testing.T) {
	r := newRecorder("")
	r.Event("code", "tools.ask(...)", nil)
	r.Event("ask", "which one?", nil)
	r.Event("result", "the first", nil)
	r.Event("cancelled", "", nil)
	calls := r.settled(t)
	texts := make([]string, 0, len(calls))
	for _, c := range calls {
		texts = append(texts, strings.SplitN(strings.TrimPrefix(c, "set-status bough "), " --", 2)[0])
	}
	if got := strings.Join(texts, "|"); got != "working|needs an answer|working|stopped" {
		t.Fatalf("pill texts: %s", got)
	}
}

// Leaving takes the pill and the description off the row.
func TestClearAtExit(t *testing.T) {
	r := newRecorder("")
	r.Event("assistant", "hi", func() string { return "say hi" })
	r.Clear()
	calls := r.settled(t)
	if n := len(calls); n != 4 || calls[2] != "clear-status bough --workspace workspace:2" ||
		calls[3] != "workspace-action --workspace workspace:2 --action clear-description" {
		t.Fatalf("calls: %v", calls)
	}
}
