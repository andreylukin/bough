package todo

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/andreylukin/bough/internal/agenttools"
)

// nativeTool is the todo list as one native tool: every op answers with
// the list as it now stands, so the model never needs a second call to
// see what it changed. The entries and the "todo" event are tools.todo's.
func (t *Todos) nativeTool() agenttools.Tool {
	return agenttools.Tool{
		Name:        "todo",
		Description: "The shared TODO list, shown to the user as you work. op add (text) adds an item, done (id) ticks one off, clear empties the list, list shows it. Every op returns the list.",
		Schema: agenttools.Object([]string{"op"}, map[string]any{
			"op":   map[string]any{"type": "string", "enum": []any{"add", "done", "clear", "list"}},
			"text": agenttools.Prop("string", "add: the item"),
			"id":   agenttools.Prop("integer", "done: the item's id"),
		}),
		Detail: func(args json.RawMessage) string {
			var a struct {
				Op   string
				Text string
				ID   int
			}
			_ = json.Unmarshal(args, &a)
			switch a.Op {
			case "add":
				return "add " + oneLine(a.Text)
			case "done":
				return fmt.Sprintf("done %d", a.ID)
			}
			return a.Op
		},
		Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
			var a struct {
				Op   string `json:"op"`
				Text string `json:"text"`
				ID   int    `json:"id"`
			}
			if err := agenttools.Decode("todo", c.Args, &a); err != nil {
				return agenttools.Result{}, err
			}
			switch a.Op {
			case "add":
				agent := c.Worker
				if _, err := t.add(a.Text, &agent); err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
			case "done":
				if err := t.Done(a.ID); err != nil {
					return agenttools.Result{Error: err.Error(), Text: t.Render()}, nil
				}
			case "clear":
				t.Clear()
			case "list":
			default:
				return agenttools.Result{Error: fmt.Sprintf("todo: op must be add, done, clear or list, got %q", a.Op)}, nil
			}
			return agenttools.Result{Text: t.Render()}, nil
		},
	}
}
