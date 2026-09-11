package replay

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// tapeMechanicsWrite writes lines as a history JSONL file and returns its path.
func tapeMechanicsWrite(t *testing.T, dir string, lines ...string) string {
	t.Helper()
	p := filepath.Join(dir, "s.jsonl")
	body := strings.Join(lines, "\n") + "\n"
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// tapeMechanicsResults builds a tape carrying only the given results,
// plus the one assistant entry Load insists on.
func tapeMechanicsResults(t *testing.T, codes ...string) *Tape {
	t.Helper()
	lines := []string{`{"seq":1,"kind":"assistant","data":{"text":"hi"}}`}
	for i, c := range codes {
		lines = append(lines, fmt.Sprintf(
			`{"seq":%d,"kind":"result","data":{"code":%q,"text":%q}}`, i+2, c, "out "+c))
	}
	tp, err := Load(tapeMechanicsWrite(t, t.TempDir(), lines...))
	if err != nil {
		t.Fatal(err)
	}
	return tp
}

// TestTapeMechanicsResultMatching covers how nextResult pairs a block
// with a recorded result: in order, re-ordered, repeated, and past the
// three-entry lookahead.
func TestTapeMechanicsResultMatching(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		codes []string // recorded results, in tape order
		run   []string // blocks the loop runs
		want  []string // what each Run should answer with
	}{
		{"in order", []string{"a", "b"}, []string{"a", "b"}, []string{"out a", "out b"}},
		{"reordered skips ahead", []string{"a", "b", "c"}, []string{"b", "c"}, []string{"out b", "out c"}},
		{"repeated block consumes both copies", []string{"a", "a"}, []string{"a", "a"}, []string{"out a", "out a"}},
		{"whitespace insensitive", []string{"a", "b"}, []string{"  a\n"}, []string{"out a"}},
		// The lookahead is three entries (xi..xi+2), so a block whose
		// result sits at xi+3 is not found and the tape falls back to
		// the next result in order.
		{"beyond lookahead falls back in order", []string{"a", "b", "c", "d"}, []string{"d"}, []string{"out a"}},
		{"unknown block falls back in order", []string{"a", "b"}, []string{"zzz"}, []string{"out a"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			rt := &Runtime{tape: tapeMechanicsResults(t, tc.codes...)}
			for i, code := range tc.run {
				out, err := rt.Run(code)
				if err != nil {
					t.Fatalf("run %d (%q): %v", i, code, err)
				}
				if out != tc.want[i] {
					t.Fatalf("run %d (%q): got %q, want %q", i, code, out, tc.want[i])
				}
			}
		})
	}
}

// TestTapeMechanicsEndOfResults is the error past the last result.
func TestTapeMechanicsEndOfResults(t *testing.T) {
	t.Parallel()
	rt := &Runtime{tape: tapeMechanicsResults(t, "a")}
	if _, err := rt.Run("a"); err != nil {
		t.Fatal(err)
	}
	if _, err := rt.Run("a"); err == nil || !strings.Contains(err.Error(), "end of tape") {
		t.Fatalf("past the last result: want an end-of-tape error, got %v", err)
	}
}

// TestTapeMechanicsErrorResults covers the shapes a recorded error
// takes: the whole result, or a trailing line after real output.
func TestTapeMechanicsErrorResults(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		text    string
		wantOut string
		wantErr string
	}{
		{"whole result", "error: exit status 1", "", "exit status 1"},
		{"trailing line keeps output", "a.go\nb.go\nerror: boom", "a.go\nb.go\n", "boom"},
		{"error word mid-line is output", "note: error: not a prefix", "note: error: not a prefix", ""},
		{"plain output", "fine\n", "fine\n", ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			tp := &Tape{Results: []result{{code: "x", text: tc.text}}}
			out, err := (&Runtime{tape: tp}).Run("x")
			if tc.wantErr == "" {
				if err != nil {
					t.Fatalf("got error %v, want output %q", err, tc.wantOut)
				}
			} else if err == nil || err.Error() != tc.wantErr {
				t.Fatalf("got error %v, want %q", err, tc.wantErr)
			}
			if out != tc.wantOut {
				t.Fatalf("output %q, want %q", out, tc.wantOut)
			}
		})
	}
}

// TestTapeMechanicsTwoFences: a reply carrying two code fences is
// replayed verbatim — the tape never edits the model's text. The loop
// runs only the first fence, so only that block's result is consumed
// and the second result stays on the tape for the next reply.
func TestTapeMechanicsTwoFences(t *testing.T) {
	t.Parallel()
	reply := "```js\\nfirst()\\n```\\ntext between\\n```js\\nsecond()\\n```"
	p := tapeMechanicsWrite(t, t.TempDir(),
		`{"seq":1,"kind":"assistant","data":{"text":"`+reply+`"}}`,
		`{"seq":2,"kind":"result","data":{"code":"first()","text":"one"}}`,
		`{"seq":3,"kind":"result","data":{"code":"second()","text":"two"}}`,
	)
	tp, err := Load(p)
	if err != nil {
		t.Fatal(err)
	}
	got, _ := (&Model{tape: tp}).Complete(t.Context(), "", nil)
	if n := strings.Count(got, "```js"); n != 2 {
		t.Fatalf("reply replayed with %d js fences, want 2: %q", n, got)
	}
	rt := &Runtime{tape: tp}
	if out, err := rt.Run("first()"); out != "one" || err != nil {
		t.Fatalf("first block: %q %v", out, err)
	}
	if out, err := rt.Run("second()"); out != "two" || err != nil {
		t.Fatalf("second block after the next reply: %q %v", out, err)
	}
}

// TestTapeMechanicsSubEntriesIgnored: sub:* entries belong to a
// subagent whose spawn block already has a recorded result, so they
// never reach the tape.
func TestTapeMechanicsSubEntriesIgnored(t *testing.T) {
	t.Parallel()
	p := tapeMechanicsWrite(t, t.TempDir(),
		`{"seq":1,"kind":"input","data":{"text":"go"}}`,
		`{"seq":2,"kind":"assistant","data":{"text":"top"}}`,
		`{"seq":3,"kind":"sub:assistant","data":{"text":"sub reply"}}`,
		`{"seq":4,"kind":"sub:result","data":{"code":"s()","text":"sub out"}}`,
		`{"seq":5,"kind":"sub:done","data":{"text":""}}`,
		`{"seq":6,"kind":"result","data":{"code":"t()","text":"top out"}}`,
	)
	tp, err := Load(p)
	if err != nil {
		t.Fatal(err)
	}
	if len(tp.Replies) != 1 || tp.Replies[0] != "top" {
		t.Fatalf("replies %q, want just the top-level one", tp.Replies)
	}
	if len(tp.Results) != 1 || tp.Results[0].text != "top out" {
		t.Fatalf("results %v, want just the top-level one", tp.Results)
	}
	if tp.Turns() != 1 {
		t.Fatalf("turns %d, want 1", tp.Turns())
	}
}

// TestTapeMechanicsLoadErrors: a tape with no assistant entry, and a
// file that is not there at all.
func TestTapeMechanicsLoadErrors(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	empty := tapeMechanicsWrite(t, dir, `{"seq":1,"kind":"input","data":{"text":"hi"}}`)
	if _, err := Load(empty); err == nil || !strings.Contains(err.Error(), "no assistant entries") {
		t.Fatalf("tape with no assistant entries: got %v", err)
	}
	if _, err := Load(filepath.Join(dir, "nope.jsonl")); err == nil {
		t.Fatal("missing file: want an error")
	}
}

// TestTapeMechanicsStreamCancel: with a delay set, Stream stops at the
// first cancellation instead of streaming the rest of the reply.
func TestTapeMechanicsStreamCancel(t *testing.T) {
	t.Parallel()
	m := &Model{tape: &Tape{Replies: []string{"one two three four five"}, Delay: 50 * time.Millisecond}}

	ctx, cancel := context.WithCancel(t.Context())
	var got []string
	done := make(chan error, 1)
	go func() {
		_, err := m.Stream(ctx, "", nil, func(d string) {
			got = append(got, d)
			if len(got) == 1 {
				cancel()
			}
		})
		done <- err
	}()
	select {
	case err := <-done:
		if err != context.Canceled {
			t.Fatalf("cancelled stream: got %v, want context.Canceled", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Stream did not return after the context was cancelled")
	}
	if len(got) != 1 {
		t.Fatalf("streamed %d deltas after cancel, want 1: %q", len(got), got)
	}
}

// TestTapeMechanicsStreamCancelledUpFront: an already-dead context
// yields no deltas at all.
func TestTapeMechanicsStreamCancelledUpFront(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	m := &Model{tape: &Tape{Replies: []string{"a b c"}}}
	n := 0
	if _, err := m.Stream(ctx, "", nil, func(string) { n++ }); err != context.Canceled {
		t.Fatalf("got %v, want context.Canceled", err)
	}
	if n != 0 {
		t.Fatalf("%d deltas on a dead context, want 0", n)
	}
}

// TestTapeMechanicsSessionID resolves `session: <id>` under $HOME.
func TestTapeMechanicsSessionID(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	id := "0192abcd-session"
	if err := os.WriteFile(filepath.Join(dir, id+".jsonl"),
		[]byte(`{"seq":1,"kind":"assistant","data":{"text":"from the session file"}}`+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, map[string]any{"session": id}); err != nil {
		t.Fatalf("session id under a fake HOME: %v", err)
	}
	m, err := kernel.Get[llm.LLM](ctx, "llm")
	if err != nil {
		t.Fatal(err)
	}
	got, err := m.Complete(t.Context(), "", nil)
	if err != nil || got != "from the session file" {
		t.Fatalf("session tape: %q %v", got, err)
	}
	if err := (plugin{}).Apply(kernel.NewContext(), map[string]any{"session": "does-not-exist"}); err == nil {
		t.Fatal("unknown session id: want an error")
	}
}

// TestTapeMechanicsDelayFromConfig: yaml hands numbers over as float64,
// a Go caller as int; both must land on Tape.Delay. The `service` key
// moves the model off "llm".
func TestTapeMechanicsDelayFromConfig(t *testing.T) {
	t.Parallel()
	p := tapeMechanicsWrite(t, t.TempDir(), `{"seq":1,"kind":"assistant","data":{"text":"hi"}}`)
	cases := []struct {
		name string
		cfg  map[string]any
		want time.Duration
	}{
		{"int", map[string]any{"delay_ms": 7}, 7 * time.Millisecond},
		{"float from yaml", map[string]any{"delay_ms": 12.0}, 12 * time.Millisecond},
		{"fractional float", map[string]any{"delay_ms": 0.5}, 500 * time.Microsecond},
		{"absent", map[string]any{}, 0},
		{"wrong type ignored", map[string]any{"delay_ms": "10"}, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			cfg := map[string]any{"file": p, "service": "llm-replay"}
			for k, v := range tc.cfg {
				cfg[k] = v
			}
			ctx := kernel.NewContext()
			if err := (plugin{}).Apply(ctx, cfg); err != nil {
				t.Fatal(err)
			}
			m, err := kernel.Get[*Model](ctx, "llm-replay")
			if err != nil {
				t.Fatalf("service key: %v", err)
			}
			if m.tape.Delay != tc.want {
				t.Fatalf("delay %v, want %v", m.tape.Delay, tc.want)
			}
		})
	}
}
