package loop

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/hooks" // registers hooks-js
)

// mcphooksinitjsLLM replies from a tape, then ends the turn.
type mcphooksinitjsLLM struct {
	mu    sync.Mutex
	tape  []string
	calls [][]Message
}

func (l *mcphooksinitjsLLM) Complete(_ context.Context, _ string, msgs []Message) (string, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.calls = append(l.calls, append([]Message(nil), msgs...))
	if len(l.tape) == 0 {
		return "```stop\nDONE\n```", nil
	}
	r := l.tape[0]
	l.tape = l.tape[1:]
	return r, nil
}

// mcphooksinitjsRunner mounts the REAL hooks-js row over a real codemode
// VM (short timeout), writes the project hook files, and returns the
// loop runner plus the tape LLM.
func mcphooksinitjsRunner(t *testing.T, hookFiles map[string]string, tape ...string) (*runner, *mcphooksinitjsLLM) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	t.Chdir(t.TempDir())
	for rel, body := range hookFiles {
		p := filepath.Join(".bough", "hooks", rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	cm := codemode.New(300 * time.Millisecond)
	kctx := kernel.NewContext()
	kctx.Provide("codemode", cm)
	if err := kctx.Mount([]kernel.Row{{ID: "hooks", Plugin: "hooks-js"}}); err != nil {
		t.Fatal(err)
	}
	h, err := kernel.Get[Hooks](kctx, "hooks")
	if err != nil {
		t.Fatal(err)
	}
	llm := &mcphooksinitjsLLM{tape: tape}
	r := buildRunner(t, &recordLLM{}, cm, h, nil)
	r.llm = llm
	t.Cleanup(kctx.Unmount)
	return r, llm
}

func mcphooksinitjsRun(t *testing.T, r *runner, input string) (kinds, texts []string) {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- r.Run(context.Background(), input, collect(&kinds, &texts)) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(20 * time.Second):
		t.Fatal("turn hung past 20s")
	}
	return kinds, texts
}

// mcphooksinitjsLastResult is what the model is fed next for the block.
func mcphooksinitjsLastResult(r *runner) string {
	out := ""
	for _, e := range r.hist.Entries() {
		if e.Kind == "result" {
			out, _ = e.Data["text"].(string)
		}
	}
	return out
}

const mcphooksinitjsBlock = "```js\nconsole.log('RAN ' + (1+1))\n```"

func TestMcpHooksInitjsHookFailures(t *testing.T) {
	cases := []struct {
		name       string
		files      map[string]string
		wantResult string // substring of the model-fed result
		notResult  string
	}{
		{"pre throws", map[string]string{"pre-code-exec/a.js": `throw new Error("pre boom")`}, "RAN 2", "pre boom"},
		{"pre times out", map[string]string{"pre-code-exec/a.js": `while (true) {}`}, "RAN 2", ""},
		{"pre returns string", map[string]string{"pre-code-exec/a.js": `return "garbage"`}, "RAN 2", ""},
		{"pre code not string", map[string]string{"pre-code-exec/a.js": `return {code: 42}`}, "RAN 2", ""},
		{"pre deny non-string", map[string]string{"pre-code-exec/a.js": `return {deny: true}`}, "RAN 2", ""},
		{"pre syntax error", map[string]string{"pre-code-exec/a.js": `return {{{`}, "RAN 2", ""},
		{"pre denies", map[string]string{"pre-code-exec/a.js": `return {deny: "nope"}`}, "[hook denied: nope]", "RAN"},
		{"pre deny then broken", map[string]string{"pre-code-exec/a.js": `return {deny: "first"}`, "pre-code-exec/b.js": `throw 1`}, "[hook denied: first]", ""},
		{"post throws", map[string]string{"post-result/a.js": `throw new Error("post boom")`}, "RAN 2", "post boom"},
		{"post times out", map[string]string{"post-result/a.js": `for(;;){}`}, "RAN 2", ""},
		{"post result non-string", map[string]string{"post-result/a.js": `return {result: 7}`}, "RAN 2", ""},
		{"post rewrites", map[string]string{"post-result/a.js": `return {result: "REWRITTEN"}`}, "REWRITTEN", "RAN"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			r, llm := mcphooksinitjsRunner(t, tc.files, mcphooksinitjsBlock)
			start := time.Now()
			kinds, texts := mcphooksinitjsRun(t, r, "go")
			if d := time.Since(start); d > 5*time.Second {
				t.Errorf("turn took %v; a hook must be bounded by the VM timeout", d)
			}
			got := mcphooksinitjsLastResult(r)
			if !strings.Contains(got, tc.wantResult) {
				t.Fatalf("model fed %q, want %q (events %v %q)", got, tc.wantResult, kinds, texts)
			}
			if tc.notResult != "" && strings.Contains(got, tc.notResult) {
				t.Fatalf("model fed %q, must not contain %q", got, tc.notResult)
			}
			if kinds[len(kinds)-1] != "done" {
				t.Fatalf("turn did not finish: %v", kinds)
			}
			last := llm.calls[len(llm.calls)-1]
			if !strings.Contains(last[len(last)-1].Content, tc.wantResult) {
				t.Fatalf("next model call lacks the result: %q", last[len(last)-1].Content)
			}
			// The session survives: a second turn runs code again.
			llm.tape = []string{"```js\nconsole.log('AGAIN')\n```"}
			mcphooksinitjsRun(t, r, "again")
			got = mcphooksinitjsLastResult(r)
			if !strings.Contains(got, "AGAIN") && !strings.Contains(got, "REWRITTEN") && !strings.Contains(got, "denied") {
				t.Fatalf("second turn result %q", got)
			}
		})
	}
}

// A user-prompt-submit hook that throws, hangs or returns garbage must
// not eat the prompt.
func TestMcpHooksInitjsPromptHookFailures(t *testing.T) {
	for name, body := range map[string]string{"throws": `throw new Error("x")`, "hangs": `while(true){}`, "garbage": `return 5`} {
		t.Run(name, func(t *testing.T) {
			r, llm := mcphooksinitjsRunner(t, map[string]string{"user-prompt-submit/a.js": body})
			mcphooksinitjsRun(t, r, "hello there")
			if len(llm.calls) == 0 || !strings.Contains(llm.calls[0][len(llm.calls[0])-1].Content, "hello there") {
				t.Fatalf("prompt did not reach the model: %v", llm.calls)
			}
		})
	}
}

// The Hooks view lives in another process and can only read history, so
// a fire that never reaches the session file is a fire nobody can see.
// This is the seam between the in-memory ledger and the web.
func TestFireIsRecordedToHistory(t *testing.T) {
	r, _ := mcphooksinitjsRunner(t, map[string]string{
		"post-result/guard.js": `return { deny: "not allowed" }`,
	}, "```js\nconsole.log(1)\n```", "done")
	mcphooksinitjsRun(t, r, "go")

	var fires []map[string]any
	for _, e := range r.hist.Entries() {
		if e.Kind == "hook" {
			fires = append(fires, e.Data)
		}
	}
	if len(fires) == 0 {
		t.Fatal("no hook entry in history: the ledger cannot reach the control room")
	}
	var denied map[string]any
	for _, f := range fires {
		if f["name"] == "guard.js" {
			denied = f
		}
	}
	if denied == nil {
		t.Fatalf("guard.js not recorded; got %v", fires)
	}
	if denied["event"] != "post-result" {
		t.Errorf("event = %v, want post-result", denied["event"])
	}
	if denied["decision"] != "denied" {
		t.Errorf("decision = %v, want denied", denied["decision"])
	}
}

// A session with no hooks installed must not pay for the ledger: no
// hook ran, so nothing is written.
func TestNoHooksWritesNothing(t *testing.T) {
	r, _ := mcphooksinitjsRunner(t, nil, "```js\nconsole.log(1)\n```", "done")
	mcphooksinitjsRun(t, r, "go")
	for _, e := range r.hist.Entries() {
		if e.Kind == "hook" {
			t.Fatalf("hook entry written with no hooks installed: %v", e.Data)
		}
	}
}
