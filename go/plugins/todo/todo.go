// Package todo is the "todo" plugin: a session TODO list derived from
// history entries at read time (kinds "todo/add", "todo/done",
// "todo/clear"), so it survives session resume the same way the
// conversation does. Surfaces: the /todo command, tools.todo in
// codemode (when mounted), and an optional "cognition" system-prompt
// section (config {inject_prompt: bool, default true}).
//
// INTEGRATOR NOTE — "cognition" is a single-slot service (kernel last
// write wins). This plugin chains rather than clobbers: at Apply it
// Gets any existing "cognition" provider and delegates to it after
// appending the TODO section (Get-tracking re-wraps on provider
// changes, and disposeRow withdraws our own provide before a reload,
// so it never wraps itself). The kernel still prints its overwrite
// WARNING, and a full-replacement cognition mounted later (or one
// that ignores its input) can still drop the TODO section; a real
// composable "prompt-section" seam is the eventual fix. Opt out with
// {inject_prompt: false}.
package todo

import (
	"fmt"
	"slices"
	"strconv"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

// Item is one TODO entry. State is "open" or "done".
type Item struct {
	ID    int
	Text  string
	State string
}

// History is the slice of the "history" service todos derive from.
// Absent, an in-memory log with the same contract is used.
type History interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
}

// toolRegistry is the slice of the codemode service we need.
type toolRegistry interface{ RegisterTool(name string, fn any) }

// memLog is the fallback History when no "history" service is mounted:
// process-local, gone at exit.
type memLog struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (m *memLog) Append(kind string, data map[string]any) history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	e := history.Entry{Seq: int64(len(m.entries) + 1), Kind: kind, Data: data}
	m.entries = append(m.entries, e)
	return e
}

func (m *memLog) Entries() []history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	return slices.Clone(m.entries)
}

// Todos is the "todo" service: state is never stored, only derived
// from the history entries on every read; mutations append entries.
type Todos struct {
	mu       sync.Mutex // makes id-allocate + append atomic
	hist     History
	emit     func(rendered string) // nil = no events
	hasTools bool                  // tools.todo registered (mentioned in the prompt section)
}

// NewTodos builds a list over hist. emit (may be nil) is called with
// the rendered list after every mutation.
func NewTodos(hist History, emit func(rendered string)) *Todos {
	return &Todos{hist: hist, emit: emit}
}

// intOf reads an id that may have round-tripped through JSON (float64)
// or come straight from Go (int/int64).
func intOf(v any) (int, bool) {
	switch n := v.(type) {
	case int:
		return n, true
	case int64:
		return int(n), true
	case float64:
		return int(n), true
	}
	return 0, false
}

// derive replays the todo/* entries: the current items plus the next
// free id (ids are never reused, even across clears).
func (t *Todos) derive() (items []Item, next int) {
	next = 1
	for _, e := range t.hist.Entries() {
		switch e.Kind {
		case "todo/add":
			id, ok := intOf(e.Data["id"])
			if !ok {
				continue
			}
			text, _ := e.Data["text"].(string)
			items = append(items, Item{ID: id, Text: text, State: "open"})
			if id >= next {
				next = id + 1
			}
		case "todo/done":
			id, ok := intOf(e.Data["id"])
			if !ok {
				continue
			}
			for i := range items {
				if items[i].ID == id {
					items[i].State = "done"
				}
			}
		case "todo/clear":
			items = nil
		}
	}
	return items, next
}

// List returns the current items, derived from history.
func (t *Todos) List() []Item {
	items, _ := t.derive()
	return items
}

// Render formats the list as checkbox lines: "[ ] 3 buy milk",
// "[x] 1 done thing"; "(no todos)" when empty.
func (t *Todos) Render() string {
	items := t.List()
	if len(items) == 0 {
		return "(no todos)"
	}
	var b strings.Builder
	for i, it := range items {
		if i > 0 {
			b.WriteByte('\n')
		}
		box := "[ ]"
		if it.State == "done" {
			box = "[x]"
		}
		fmt.Fprintf(&b, "%s %d %s", box, it.ID, it.Text)
	}
	return b.String()
}

// Add appends an open item and returns its id.
func (t *Todos) Add(text string) (int, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return 0, fmt.Errorf("todo: empty text")
	}
	t.mu.Lock()
	_, id := t.derive()
	t.hist.Append("todo/add", map[string]any{"id": id, "text": text})
	t.mu.Unlock()
	t.notify()
	return id, nil
}

// Done marks an open item done; an unknown or already-done id is an
// error.
func (t *Todos) Done(id int) error {
	t.mu.Lock()
	items, _ := t.derive()
	found := false
	for _, it := range items {
		if it.ID == id && it.State == "open" {
			found = true
		}
	}
	if !found {
		t.mu.Unlock()
		return fmt.Errorf("todo: no open item %d", id)
	}
	t.hist.Append("todo/done", map[string]any{"id": id})
	t.mu.Unlock()
	t.notify()
	return nil
}

// Clear removes every item.
func (t *Todos) Clear() {
	t.mu.Lock()
	t.hist.Append("todo/clear", nil)
	t.mu.Unlock()
	t.notify()
}

func (t *Todos) notify() {
	if t.emit != nil {
		t.emit(t.Render())
	}
}

// runCommand is /todo: no args renders the list; "add <text>",
// "done <id>", "clear" mutate and render the result.
func (t *Todos) runCommand(args string) (string, error) {
	args = strings.TrimSpace(args)
	switch {
	case args == "":
		return t.Render(), nil
	case args == "clear":
		t.Clear()
		return t.Render(), nil
	case args == "add" || strings.HasPrefix(args, "add "):
		if _, err := t.Add(strings.TrimPrefix(args, "add")); err != nil {
			return "", err
		}
		return t.Render(), nil
	case args == "done" || strings.HasPrefix(args, "done "):
		id, err := strconv.Atoi(strings.TrimSpace(strings.TrimPrefix(args, "done")))
		if err != nil {
			return "", fmt.Errorf("todo: bad id %q", strings.TrimSpace(strings.TrimPrefix(args, "done")))
		}
		if err := t.Done(id); err != nil {
			return "", err
		}
		return t.Render(), nil
	default:
		return "", fmt.Errorf("usage: /todo [add <text> | done <id> | clear]")
	}
}

// prevCognition is the shape of an already-mounted "cognition"
// provider this one chains to.
type prevCognition interface{ System(base string) string }

// cognition appends the live TODO section to the system prompt, then
// delegates to the provider it replaced (if any) so both transforms
// apply. See the package note on the single-slot "cognition" tension.
type cognition struct {
	t    *Todos
	prev prevCognition // nil = no prior provider
}

func (c cognition) System(base string) string {
	s := base + "\n\nCurrent TODO list:\n" + c.t.Render()
	if c.t.hasTools {
		s += "\n\nYou may manage this list from code: tools.todo.add(text) -> id, tools.todo.done(id), tools.todo.list() -> rendered list."
	}
	if c.prev != nil {
		return c.prev.System(s)
	}
	return s
}

type plugin struct{}

func init() {
	kernel.Register("todo", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "todo" }
func (plugin) Inject() []string { return []string{"commands"} }

// Apply mounts the todo list: derives from the "history" service when
// present (memLog otherwise), registers /todo, registers tools.todo
// when a codemode service is present, provides "todo", and — unless
// {inject_prompt: false} — provides "cognition".
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	inject := true
	if v, has := cfg["inject_prompt"]; has {
		b, ok := v.(bool)
		if !ok {
			return fmt.Errorf("todo: inject_prompt must be a bool, got %v", v)
		}
		inject = b
	}
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}

	var h History = &memLog{}
	if hs, err := kernel.Get[History](ctx, "history"); err == nil {
		h = hs
	}

	t := NewTodos(h, func(rendered string) {
		ctx.Emit("loop/event", map[string]string{"Kind": "todo", "Text": rendered})
	})

	if cm, err := kernel.Get[toolRegistry](ctx, "codemode"); err == nil {
		cm.RegisterTool("todo", map[string]any{
			"add": func(text string) (int, error) { return t.Add(text) },
			"done": func(id int) (string, error) {
				if err := t.Done(id); err != nil {
					return "", err
				}
				return t.Render(), nil
			},
			"list": func() string { return t.Render() },
		})
		if d, ok := any(cm).(interface{ Describe(name, line string) }); ok {
			d.Describe("todo", `tools.todo.add(text) -> id, tools.todo.done(id), tools.todo.list() -> string: the shared TODO list, shown to the user as you work.`)
		}
		t.hasTools = true
		ctx.Effect(func() { cm.RegisterTool("todo", nil) })
	}

	info := commands.CommandInfo{Name: "todo", Usage: "[add <text> | done <id> | clear]", Summary: "manage the TODO list"}
	if err := reg.Register(info, t.runCommand); err != nil {
		return err
	}
	ctx.Effect(func() { reg.Unregister("todo") })

	if inject {
		cog := cognition{t: t}
		if prev, err := kernel.Get[prevCognition](ctx, "cognition"); err == nil {
			cog.prev = prev
		}
		ctx.Provide("cognition", cog)
	}
	ctx.Provide("todo", t)
	return nil
}
