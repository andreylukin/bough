package todo

import (
	"context"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// TestDeriveFromHistoryRoundTrip mutates over a real JSONL store, then
// resumes the file (JSON round-trip: ids come back as float64) and
// checks the derived state survives, including the next id.
func TestDeriveFromHistoryRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "s.jsonl")
	st, err := history.Open(path)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	td := NewTodos(st, nil)
	id1, err := td.Add("buy milk")
	if err != nil {
		t.Fatalf("add: %v", err)
	}
	if _, err := td.Add("walk dog"); err != nil {
		t.Fatalf("add: %v", err)
	}
	if err := td.Done(id1); err != nil {
		t.Fatalf("done: %v", err)
	}
	if err := st.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}

	st2, err := history.OpenExisting(path)
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	defer st2.Close()
	td2 := NewTodos(st2, nil)
	if got, want := td2.Render(), "[x] 1 buy milk\n[ ] 2 walk dog"; got != want {
		t.Fatalf("after resume: %q, want %q", got, want)
	}
	id3, err := td2.Add("third")
	if err != nil {
		t.Fatalf("add after resume: %v", err)
	}
	if id3 != 3 {
		t.Fatalf("id after resume = %d, want 3", id3)
	}
	td2.Clear()
	if got := td2.Render(); got != "(no todos)" {
		t.Fatalf("after clear: %q", got)
	}
	// Ids are never reused, even across clears.
	if id, _ := td2.Add("fresh"); id != 4 {
		t.Fatalf("id after clear = %d, want 4", id)
	}
}

func TestDoneUnknownID(t *testing.T) {
	td := NewTodos(&memLog{}, nil)
	if err := td.Done(7); err == nil {
		t.Fatal("Done(7) on empty list: want error")
	}
	id, _ := td.Add("x")
	if err := td.Done(id); err != nil {
		t.Fatalf("done: %v", err)
	}
	if err := td.Done(id); err == nil {
		t.Fatal("double Done: want error")
	}
}

// mountRows mounts the named plugin rows on a fresh context.
func mountRows(t *testing.T, ctx *kernel.Context, rows ...kernel.Row) {
	t.Helper()
	if err := ctx.Mount(rows); err != nil {
		t.Fatalf("mount: %v", err)
	}
	t.Cleanup(ctx.Unmount)
}

func TestTodoCommand(t *testing.T) {
	ctx := kernel.NewContext()
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		t.Fatalf("commands: %v", err)
	}

	if out, err := reg.Run("todo", ""); err != nil || out != "(no todos)" {
		t.Fatalf("/todo = %q, %v", out, err)
	}
	if out, err := reg.Run("todo", "add buy milk"); err != nil || out != "[ ] 1 buy milk" {
		t.Fatalf("/todo add = %q, %v", out, err)
	}
	if out, err := reg.Run("todo", "add walk dog"); err != nil || out != "[ ] 1 buy milk\n[ ] 2 walk dog" {
		t.Fatalf("/todo add #2 = %q, %v", out, err)
	}
	if out, err := reg.Run("todo", "done 1"); err != nil || out != "[x] 1 buy milk\n[ ] 2 walk dog" {
		t.Fatalf("/todo done = %q, %v", out, err)
	}
	if _, err := reg.Run("todo", "done xyz"); err == nil {
		t.Fatal("/todo done xyz: want error")
	}
	if _, err := reg.Run("todo", "bogus"); err == nil {
		t.Fatal("/todo bogus: want usage error")
	}
	if out, err := reg.Run("todo", "clear"); err != nil || out != "(no todos)" {
		t.Fatalf("/todo clear = %q, %v", out, err)
	}
}

// codeRunner is the slice of the codemode service the test drives.
type codeRunner interface {
	Run(code string) (string, error)
}

func TestToolsTodoViaCodemode(t *testing.T) {
	ctx := kernel.NewContext()
	mountRows(t, ctx,
		kernel.Row{ID: "codemode", Plugin: "codemode"},
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	cm, err := kernel.Get[codeRunner](ctx, "codemode")
	if err != nil {
		t.Fatalf("codemode: %v", err)
	}
	out, err := cm.Run(`console.log(tools.todo.add("buy milk")); console.log(tools.todo.add("walk dog"))`)
	if err != nil {
		t.Fatalf("add: %v", err)
	}
	if out != "1\n2\n" {
		t.Fatalf("add ids = %q, want \"1\\n2\\n\"", out)
	}
	out, err = cm.Run(`tools.todo.done(1)`)
	if err != nil {
		t.Fatalf("done: %v", err)
	}
	if out != "[x] 1 buy milk\n[ ] 2 walk dog" {
		t.Fatalf("done = %q", out)
	}
	out, err = cm.Run(`tools.todo.list()`)
	if err != nil || out != "[x] 1 buy milk\n[ ] 2 walk dog" {
		t.Fatalf("list = %q, %v", out, err)
	}
	// A bad id surfaces as a JS exception, not a silent no-op.
	if _, err := cm.Run(`tools.todo.done(99)`); err == nil {
		t.Fatal("done(99): want error")
	}
}

// recordLLM records the system prompt and ends every turn with plain
// text.
type recordLLM struct{ system string }

func (l *recordLLM) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	l.system = system
	return "ok", nil
}

// runnerIface is the slice of the loop's "runner" service.
type runnerIface interface {
	Run(ctx context.Context, input string, emit func(kind, text string)) error
}

func TestCognitionInjectsIntoSystemPrompt(t *testing.T) {
	ctx := kernel.NewContext()
	stub := &recordLLM{}
	ctx.Provide("llm", stub)
	mountRows(t, ctx,
		kernel.Row{ID: "codemode", Plugin: "codemode"},
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo"},
		kernel.Row{ID: "loop", Plugin: "loop"},
	)
	td, err := kernel.Get[*Todos](ctx, "todo")
	if err != nil {
		t.Fatalf("todo: %v", err)
	}
	if _, err := td.Add("ship it"); err != nil {
		t.Fatalf("add: %v", err)
	}
	r, err := kernel.Get[runnerIface](ctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	if err := r.Run(t.Context(), "hello", func(kind, text string) {}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if !strings.Contains(stub.system, "Current TODO list:\n[ ] 1 ship it") {
		t.Fatalf("system prompt missing todo section:\n%s", stub.system)
	}
	if !strings.Contains(stub.system, "tools.todo.add") {
		t.Fatalf("system prompt missing tools.todo instruction:\n%s", stub.system)
	}
}

// appendCog is a stand-in for a prior cognition provider (init-js's
// system.append, say).
type appendCog struct{ suffix string }

func (a appendCog) System(base string) string { return base + "\n" + a.suffix }

// TestCognitionChainsPriorProvider mounts todo over an existing
// "cognition" provider and checks both transforms apply, with the
// prior provider outermost (its append stays at the very end).
func TestCognitionChainsPriorProvider(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("cognition", appendCog{suffix: "MARK_AT_END"})
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	cog, err := kernel.Get[prevCognition](ctx, "cognition")
	if err != nil {
		t.Fatalf("cognition: %v", err)
	}
	out := cog.System("BASE")
	if !strings.Contains(out, "Current TODO list:") {
		t.Fatalf("missing todo section: %q", out)
	}
	if !strings.HasPrefix(out, "BASE") || !strings.HasSuffix(out, "MARK_AT_END") {
		t.Fatalf("chain order wrong: %q", out)
	}
}

func TestInjectPromptFalse(t *testing.T) {
	ctx := kernel.NewContext()
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo", Config: map[string]any{"inject_prompt": false}},
	)
	if _, err := kernel.Get[any](ctx, "cognition"); err == nil {
		t.Fatal("cognition provided despite inject_prompt: false")
	}
}

// TestMutationEmitsEvent checks every mutation emits a "todo"
// loop/event carrying the rendered list.
func TestMutationEmitsEvent(t *testing.T) {
	ctx := kernel.NewContext()
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	var got []string
	ctx.On("loop/event", func(payload any) {
		m, ok := payload.(map[string]string)
		if !ok || m["Kind"] != "todo" {
			t.Errorf("payload = %#v, want map with Kind todo", payload)
			return
		}
		got = append(got, m["Text"])
	})
	td, err := kernel.Get[*Todos](ctx, "todo")
	if err != nil {
		t.Fatalf("todo: %v", err)
	}
	id, _ := td.Add("a")
	_ = td.Done(id)
	td.Clear()
	want := []string{"[ ] 1 a", "[x] 1 a", "(no todos)"}
	if len(got) != len(want) {
		t.Fatalf("events = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("event %d = %q, want %q", i, got[i], want[i])
		}
	}
}
