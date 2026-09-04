package loop

import (
	"context"
	"errors"
	"os"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/kernel"
)

// stubLLM replies with a js block on the first call, plain text after.
type stubLLM struct{ calls int }

func (s *stubLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	s.calls++
	if s.calls == 1 {
		return "Running it.\n```js\nconsole.log('CODE!')\n```", nil
	}
	return "All done.", nil
}

type stubCode struct{ ran []string }

func (s *stubCode) RegisterTool(name string, fn any) {}
func (s *stubCode) Run(code string) (string, error) {
	s.ran = append(s.ran, code)
	return "CODE!", nil
}

func TestLoopCodeResultDoneSequence(t *testing.T) {
	kctx := kernel.NewContext()
	kctx.Provide("llm", &stubLLM{})
	code := &stubCode{}
	kctx.Provide("codemode", code)

	var mu sync.Mutex
	var kinds []string
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		ev, ok := p.(Event)
		if !ok {
			t.Errorf("payload is %T, want Event", p)
			return
		}
		mu.Lock()
		kinds = append(kinds, ev.Kind)
		mu.Unlock()
		if ev.Kind == "done" {
			close(done)
		}
	})

	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	inputs, err := kernel.Get[chan string](kctx, "inputs")
	if err != nil {
		t.Fatalf("inputs: %v", err)
	}
	inputs <- "do the thing"

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for done event")
	}

	mu.Lock()
	defer mu.Unlock()
	want := []string{"assistant", "code", "result", "assistant", "done"}
	if len(kinds) != len(want) {
		t.Fatalf("kinds = %v, want %v", kinds, want)
	}
	for i := range want {
		if kinds[i] != want[i] {
			t.Fatalf("kinds = %v, want %v", kinds, want)
		}
	}
	if len(code.ran) != 1 || code.ran[0] != "console.log('CODE!')\n" {
		t.Fatalf("codemode ran %q", code.ran)
	}

	// The turn is recorded in history: input first, then the event
	// mirror (a run's output lands as a "result" entry either way).
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	var entryKinds []string
	for _, e := range r.hist.Entries() {
		entryKinds = append(entryKinds, e.Kind)
	}
	wantEntries := []string{"input", "assistant", "code", "result", "assistant", "done"}
	if len(entryKinds) != len(wantEntries) {
		t.Fatalf("entry kinds = %v, want %v", entryKinds, wantEntries)
	}
	for i := range wantEntries {
		if entryKinds[i] != wantEntries[i] {
			t.Fatalf("entry kinds = %v, want %v", entryKinds, wantEntries)
		}
	}
	entries := r.hist.Entries()
	if entries[0].Data["text"] != "do the thing" {
		t.Fatalf("input entry = %+v", entries[0])
	}
	if entries[3].Data["code"] != "console.log('CODE!')\n" {
		t.Fatalf("result entry carries no code: %+v", entries[3])
	}
	kctx.Unmount()
}

// sysLLM records the system prompt it was called with.
type sysLLM struct {
	mu     sync.Mutex
	system string
}

func (s *sysLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	s.mu.Lock()
	s.system = system
	s.mu.Unlock()
	return "ok", nil
}

func (s *sysLLM) seen() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.system
}

// runTurn mounts the loop over llm/code (plus extra services), sends
// one input and waits for its done event.
func runTurn(t *testing.T, llm LLM, extra map[string]any) {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", &stubCode{})
	for k, v := range extra {
		kctx.Provide(k, v)
	}
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(Event); ok && ev.Kind == "done" {
			select {
			case <-done:
			default:
				close(done)
			}
		}
	})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	inputs, err := kernel.Get[chan string](kctx, "inputs")
	if err != nil {
		t.Fatalf("inputs: %v", err)
	}
	inputs <- "hi"
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for done event")
	}
	kctx.Unmount()
}

func TestSystemPromptDocumentsAskWhenMounted(t *testing.T) {
	llm := &sysLLM{}
	runTurn(t, llm, map[string]any{"ask-answers": struct{}{}})
	sys := llm.seen()
	for _, want := range []string{"tools.ask(", "separate argument", "clickable choices"} {
		if !strings.Contains(sys, want) {
			t.Errorf("system prompt should contain %q:\n%s", want, sys)
		}
	}
}

func TestSystemPromptOmitsAskWhenAbsent(t *testing.T) {
	llm := &sysLLM{}
	runTurn(t, llm, nil)
	if strings.Contains(llm.seen(), "tools.ask(") {
		t.Errorf("no ask plugin mounted, prompt must not document tools.ask:\n%s", llm.seen())
	}
}

// truncate keeps both ends: a tail-only cut threw away three of six
// subagent reports and left the third mid-sentence.
func TestTruncateKeepsBothEnds(t *testing.T) {
	s := "HEAD" + strings.Repeat("x", 5000) + "TAIL"
	got := truncate(s, 400)
	if !strings.HasPrefix(got, "HEAD") {
		t.Fatalf("the head must survive: %.20q", got)
	}
	if !strings.HasSuffix(got, "TAIL") {
		t.Fatalf("the tail must survive: %q", got[len(got)-20:])
	}
	if !strings.Contains(got, "bytes cut") {
		t.Fatalf("the cut must be named: %q", got)
	}
	if len(got) > 500 {
		t.Fatalf("the cut must actually cut: %d bytes", len(got))
	}
}

// truncate never splits a multi-byte rune.
func TestTruncateKeepsUTF8Whole(t *testing.T) {
	s := strings.Repeat("日", 10) // 30 bytes
	for n := 1; n < 30; n++ {
		out := truncate(s, n)
		if !utf8.ValidString(out) {
			t.Fatalf("n=%d: invalid UTF-8 %q", n, out)
		}
		if !strings.Contains(out, "bytes cut") {
			t.Fatalf("n=%d: no marker: %q", n, out)
		}
	}
}

// stepLLM answers with a code block until it is told the budget is
// spent, then with plain text. It records the last user message.
type stepLLM struct {
	final string
	last  string
}

func (l *stepLLM) Complete(_ context.Context, _ string, msgs []Message) (string, error) {
	for i := len(msgs) - 1; i >= 0; i-- {
		if msgs[i].Role == "user" {
			l.last = msgs[i].Content
			break
		}
	}
	if strings.Contains(l.last, "out of steps") {
		return l.final, nil
	}
	return "working\n```js\nconsole.log(1)\n```", nil
}

// failCode prints, then fails — a block whose first call succeeded and
// whose second threw.
type failCode struct{ out string }

func (failCode) RegisterTool(string, any) {}
func (f failCode) Run(string) (string, error) {
	return f.out, errors.New("GoError: tls: bad record MAC")
}

// A turn that spends its whole step budget still answers: the last call
// asks for one, no block runs, and the turn ends "done" without error.
func TestSpentBudgetStillAnswers(t *testing.T) {
	l := &stepLLM{final: "I ran out of room. I read the kernel; the plugins are next."}
	kctx := kernel.NewContext()
	kctx.Provide("llm", l)
	kctx.Provide("codemode", &stubCode{})
	if err := (&plugin{}).Apply(kctx, map[string]any{"max_steps": 3}); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}

	var kinds, texts []string
	if err := r.Run(context.Background(), "explain the codebase", collect(&kinds, &texts)); err != nil {
		t.Fatalf("a spent budget must not be an error: %v", err)
	}
	if kinds[len(kinds)-1] != "done" || kinds[len(kinds)-2] != "assistant" {
		t.Fatalf("the turn must end with an answer then done, got %v", kinds)
	}
	if !strings.Contains(texts[len(texts)-2], "ran out of room") {
		t.Fatalf("the final answer is the turn's last message, got %q", texts[len(texts)-2])
	}
	if !strings.Contains(l.last, "out of steps") {
		t.Fatalf("the final call must say the budget is spent: %q", l.last)
	}
}

// Everything a failing block printed before it threw reaches the model:
// a block that logs one subagent's report and then throws must not lose
// the report.
func TestFailedBlockKeepsWhatItPrinted(t *testing.T) {
	l := &recordLLM{reply: "go\n```js\nboom()\n```"}
	r := buildRunner(t, l, failCode{out: "REPORT FROM THE FIRST CHILD"}, nil, nil)
	r.maxSteps = 1

	var kinds, texts []string
	_ = r.Run(context.Background(), "go", collect(&kinds, &texts))
	var result string
	for i, k := range kinds {
		if k == "error" || k == "result" {
			result = texts[i]
		}
	}
	if !strings.Contains(result, "REPORT FROM THE FIRST CHILD") {
		t.Fatalf("partial output lost: %q", result)
	}
	if !strings.Contains(result, "bad record MAC") {
		t.Fatalf("the error must still be reported: %q", result)
	}
}

// The model is told where it is and what userland it has: without this
// a turn opens with `pwd` and reaches for GNU flags on macOS.
func TestSystemPromptNamesCwdAndPlatform(t *testing.T) {
	l := &recordLLM{}
	r := buildRunner(t, l, &stubCode{}, nil, nil)
	if err := r.Run(context.Background(), "hi", func(string, string) {}); err != nil {
		t.Fatalf("Run: %v", err)
	}
	wd, _ := os.Getwd()
	for _, want := range []string{wd, runtime.GOOS, "Working directory"} {
		if !strings.Contains(l.system, want) {
			t.Fatalf("system prompt lacks %q:\n%s", want, l.system)
		}
	}
}

// A block that prints nothing says so. An empty "[tool output]" reads
// to a model as a broken runtime, not as its own missing console.log.
func TestSilentBlockSaysSo(t *testing.T) {
	if got := noneNoted(""); !strings.Contains(got, "printed nothing") {
		t.Fatalf("empty output must be named: %q", got)
	}
	if got := noneNoted("  \n "); !strings.Contains(got, "printed nothing") {
		t.Fatalf("whitespace-only output must be named: %q", got)
	}
	if got := noneNoted("real"); got != "real" {
		t.Fatalf("real output passes through: %q", got)
	}
}

// The prompt must rule out async/await: goja has no event loop, and a
// model asked for "parallel" work writes `await Promise.all(...)`,
// which is a syntax error that kills the entire block.
func TestSystemPromptRulesOutAwait(t *testing.T) {
	for _, want := range []string{"synchronous", "await", "Promise"} {
		if !strings.Contains(SystemPrompt, want) {
			t.Fatalf("system prompt must mention %q", want)
		}
	}
}
