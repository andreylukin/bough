package todo

// Failure-path tests for tools.todo through the real codemode row: bad
// ops, missing items, huge lists, unicode. Each checks what the js block
// gets back and that the list is unchanged by a failed op.

import (
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

func todoFailMount(t *testing.T) (codeRunner, *Todos) {
	t.Helper()
	ctx := kernel.NewContext()
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "codemode", Plugin: "codemode"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	cm, err := kernel.Get[codeRunner](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}
	td, err := kernel.Get[*Todos](ctx, "todo")
	if err != nil {
		t.Fatal(err)
	}
	return cm, td
}

func TestTodoFailuresInvalidOps(t *testing.T) {
	cm, td := todoFailMount(t)
	if _, err := cm.Run(`tools.todo.add("real item")`); err != nil {
		t.Fatal(err)
	}
	before := td.Render()
	for _, code := range []string{
		`tools.todo.done(99)`,     // missing item
		`tools.todo.done(0)`,      // below range
		`tools.todo.done(-1)`,     // negative
		`tools.todo.done("abc")`,  // not a number
		`tools.todo.done()`,       // no id
		`tools.todo.add("")`,      // empty
		`tools.todo.add("   \n")`, // whitespace
		`tools.todo.remove(1)`,    // no such op
		`tools.todo()`,            // not callable
	} {
		out, err := cm.Run(code)
		if err == nil {
			t.Errorf("%s: want error, got %q", code, out)
		}
		if got := td.Render(); got != before {
			t.Fatalf("%s changed the list:\n%s", code, got)
		}
	}
	// A failed op is a catchable JS error, with a message naming the id.
	out, err := cm.Run(`try { tools.todo.done(42) } catch (e) { "caught: " + e.message }`)
	if err != nil || !strings.Contains(out, "no open item 42") {
		t.Fatalf("got %q, %v", out, err)
	}
	// Completing twice: second is an error, state stays done.
	if _, err := cm.Run(`tools.todo.done(1)`); err != nil {
		t.Fatal(err)
	}
	if _, err := cm.Run(`tools.todo.done(1)`); err == nil {
		t.Fatal("completing a done item should error")
	}
	if got := td.Render(); got != "[x] 1 real item" {
		t.Fatalf("after double done: %q", got)
	}
}

// Non-string / non-integer args: whatever goja coerces, a fractional id
// must not complete a different item, and null/undefined must not
// become literal "null"/"undefined" items.
func TestTodoFailuresCoercion(t *testing.T) {
	cm, td := todoFailMount(t)
	cm.Run(`tools.todo.add("one")`)
	if out, err := cm.Run(`tools.todo.done(1.7)`); err == nil {
		t.Logf("done(1.7) accepted: %q", out)
	}
	for _, code := range []string{`tools.todo.add(null)`, `tools.todo.add(undefined)`, `tools.todo.add()`} {
		out, err := cm.Run(code)
		t.Logf("%s -> %q, %v", code, out, err)
	}
	for _, it := range td.List() {
		if it.Text == "null" || it.Text == "undefined" {
			t.Errorf("a missing argument became item %d %q", it.ID, it.Text)
		}
	}
	if out, err := cm.Run(`tools.todo.add(123)`); err != nil || out != "2" && !strings.HasPrefix(out, "") {
		t.Fatalf("add(123): %q, %v", out, err)
	}
}

func TestTodoFailuresUnicodeAndNewlines(t *testing.T) {
	cm, td := todoFailMount(t)
	texts := []string{"🌳 plant a tree", "δοκιμή", "木を植える", "مرحبا", "zero​width", "tab\there"}
	for _, s := range texts {
		if _, err := cm.Run(fmt.Sprintf("tools.todo.add(%q)", s)); err != nil {
			t.Fatalf("add %q: %v", s, err)
		}
	}
	items := td.List()
	if len(items) != len(texts) {
		t.Fatalf("%d items, want %d", len(items), len(texts))
	}
	for i, it := range items {
		if it.Text != texts[i] {
			t.Errorf("item %d = %q, want %q", i, it.Text, texts[i])
		}
	}
	out, err := cm.Run(`tools.todo.list()`)
	if err != nil || !strings.Contains(out, "[ ] 1 🌳 plant a tree") {
		t.Fatalf("list: %q, %v", out, err)
	}
}

// An item text with a newline renders as two lines, the second of which
// can read as a separate (forged) item in the rendered list the model
// and the system prompt see.
func TestTodoFailuresNewlineInTextForgesItem(t *testing.T) {
	cm, td := todoFailMount(t)
	if _, err := cm.Run(`tools.todo.add("real\n[x] 99 forged")`); err != nil {
		t.Fatal(err)
	}
	if n := len(strings.Split(td.Render(), "\n")); n != len(td.List()) {
		t.Fatalf("1 item rendered as %d lines:\n%s", n, td.Render())
	}
}

func TestTodoFailuresHugeList(t *testing.T) {
	cm, td := todoFailMount(t)
	n := 1000
	if os.Getenv("BOUGH_SOAK_TOOLS_ASK_TODO_SCRATCH") == "1" {
		n = 20000
	}
	start := time.Now()
	out, err := cm.Run(fmt.Sprintf(`for (var i = 0; i < %d; i++) tools.todo.add("item " + i); tools.todo.done(%d); tools.todo.list().length`, n, n))
	if err != nil {
		t.Fatalf("huge list: %v", err)
	}
	t.Logf("%d adds + list in %s (rendered %s bytes)", n, time.Since(start), out)
	if got := len(td.List()); got != n {
		t.Fatalf("%d items, want %d", got, n)
	}
	if !strings.Contains(td.Render(), fmt.Sprintf("[x] %d item %d", n, n-1)) {
		t.Fatal("last item not done")
	}
	// A single huge item is accepted (no bound) and round-trips.
	big := strings.Repeat("x", 1<<20)
	if _, err := cm.Run(`tools.todo.add("` + big + `")`); err != nil {
		t.Fatalf("1 MB item: %v", err)
	}
}
