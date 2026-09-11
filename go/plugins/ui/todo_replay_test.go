package ui

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/todo"
)

// todoLog is a todo.History over a fixed entry list.
type todoLog struct{ entries []history.Entry }

func (l *todoLog) Append(kind string, data map[string]any) history.Entry {
	e := history.Entry{Seq: int64(len(l.entries) + 1), Kind: kind, Data: data}
	l.entries = append(l.entries, e)
	return e
}
func (l *todoLog) Entries() []history.Entry { return l.entries }

// Switching from a session with a todo list to one without must not
// keep the old session's panel: replay reads the (now empty) service.
func TestReplayClearsStaleTodoOnSwitch(t *testing.T) {
	m := testModel(t)
	cfg := m.cfg.Load()
	cfg.hist = histWith("/tmp/s.jsonl", "hi")
	log := &todoLog{}
	cfg.todo = todo.NewTodos(log, nil)
	m.cfg.Store(cfg)

	if _, err := cfg.todo.Add("old item"); err != nil {
		t.Fatal(err)
	}
	m.replay()
	if !strings.Contains(m.todoText, "old item") {
		t.Fatalf("resume should pin the list, got %q", m.todoText)
	}

	log.entries = nil // the switched-to session has no todo entries
	m.blocks = nil
	m.replay()
	if m.todoText != "" {
		t.Fatalf("stale todo panel after switch: %q", m.todoText)
	}
}

// A hot reload that removes the todo row remounts the ui with no todo
// service; the panel goes with it (the model still holds the text).
func TestTodoPanelGoesWithItsRow(t *testing.T) {
	d := defaultDrv(t)
	d.event("todo", "[ ] 1 ship it")
	if !strings.Contains(d.plain(), "todo · ctrl+t hides") {
		t.Fatalf("the todo panel should show:\n%s", d.plain())
	}
	cfg := *d.cfgp.Load()
	cfg.todo = nil
	d.cfgp.Store(&cfg)
	if p := d.plain(); strings.Contains(p, "todo · ctrl+t hides") {
		t.Fatalf("todo panel outlives its row:\n%s", p)
	}
}
