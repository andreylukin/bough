package todo

import (
	"context"
	"encoding/json"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/agenttools"
)

// The native todo is the same list as tools.todo: every op answers with
// the list, a subagent's item carries its name, and the todo event
// fires on each change.
func TestNativeTodo(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	var events []string
	ctx.On("loop/event", func(p any) {
		if m, ok := p.(map[string]string); ok && m["Kind"] == "todo" {
			events = append(events, m["Text"])
		}
	})
	mountRows(t, ctx,
		kernel.Row{ID: "commands", Plugin: "commands"},
		kernel.Row{ID: "agent-tools", Plugin: "agent-tools"},
		kernel.Row{ID: "todo", Plugin: "todo"},
	)
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		t.Fatal(err)
	}
	tl, ok := reg.Lookup("todo")
	if !ok {
		t.Fatal("no native todo")
	}
	run := func(worker, args string) agenttools.Result {
		t.Helper()
		r, err := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(args), Worker: worker})
		if err != nil {
			t.Fatal(err)
		}
		return r
	}
	if r := run("", `{"op":"add","text":"write the test"}`); r.Text != "[ ] 1 write the test" {
		t.Fatalf("add = %+v", r)
	}
	if r := run("subagent 1", `{"op":"add","text":"read the docs"}`); r.Text != "[ ] 1 write the test\n[ ] 2 read the docs · subagent 1" {
		t.Fatalf("worker add = %+v", r)
	}
	if r := run("", `{"op":"done","id":1}`); r.Text != "[x] 1 write the test\n[ ] 2 read the docs · subagent 1" {
		t.Fatalf("done = %+v", r)
	}
	if r := run("", `{"op":"done","id":9}`); r.Error != "todo: no open item 9" {
		t.Fatalf("done 9 = %+v", r)
	}
	if r := run("", `{"op":"list"}`); r.Error != "" || r.Text == "" {
		t.Fatalf("list = %+v", r)
	}
	if r := run("", `{"op":"clear"}`); r.Text != "(no todos)" {
		t.Fatalf("clear = %+v", r)
	}
	if r := run("", `{"op":"juggle"}`); r.Error == "" {
		t.Fatal("unknown op accepted")
	}
	if d := tl.Detail(json.RawMessage(`{"op":"done","id":3}`)); d != "done 3" {
		t.Fatalf("detail = %q", d)
	}
	if len(events) != 4 {
		t.Fatalf("todo events = %d, want one per change (add, add, done, clear)", len(events))
	}
}
