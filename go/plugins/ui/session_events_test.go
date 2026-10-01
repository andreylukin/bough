package ui

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/commands"
)

// /new runs inside Update, while waitEvent can already have handed an
// old delta to Bubble Tea. Deliver that message after the command, with
// no renderer, sleeps or scheduler race to hide which session it reaches.
func TestNewSessionRejectsQueuedEvents(t *testing.T) {
	t.Parallel()
	for _, batch := range []bool{false, true} {
		name := "single"
		if batch {
			name = "batch"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			old := cfgWith(t, nil, nil, fakeHist{path: filepath.Join(dir, "old.jsonl")})
			fresh := cfgWith(t, nil, nil, fakeHist{path: filepath.Join(dir, "fresh.jsonl")})
			r := commands.NewRegistry()
			old.cmds = r
			d := newDrv(t, 100, 30, old)
			events := make(chan Event, 2)
			d.m.events = events
			if err := r.Register(commands.CommandInfo{Name: "new"}, func(string) (string, error) {
				d.cfgp.Store(fresh)
				// The new mount can emit before perform(ActionClear)
				// replays it. That queued event must survive the swap.
				events <- fresh.eventOf(Event{Kind: "system", Text: "fresh session notice"})
				return "", commands.ActionClear
			}); err != nil {
				t.Fatal(err)
			}
			d.typeStr("start the long one")
			d.feed(keyEnter())
			d.feed(eventMsg(old.eventOf(Event{Kind: "assistant-delta", Text: "ALPHASTART "})))
			events <- old.eventOf(Event{Kind: "assistant-delta", Text: "filler"})
			if batch {
				events <- old.eventOf(Event{Kind: "job", Text: "old job", Data: map[string]any{"wake": true}})
			}
			queued := d.m.waitEvent()()
			d.typeStr("/new")
			d.feed(keyEnter())
			if d.m.sessID != "fresh" || len(d.m.blocks) != 0 || d.m.running {
				t.Fatalf("/new did not reset the pane: session=%q running=%v\n%s", d.m.sessID, d.m.running, d.plain())
			}
			d.feed(queued)
			if s := d.plain(); strings.Contains(s, "filler") || strings.Contains(s, "old job") || len(d.m.blocks) != 0 || d.m.running {
				t.Fatalf("queued old events reached the new pane (running=%v):\n%s", d.m.running, s)
			}
			d.feed(d.m.waitEvent()())
			if s := d.plain(); !strings.Contains(s, "fresh session notice") {
				t.Fatalf("an event queued by the new mount was lost:\n%s", s)
			}
		})
	}
}

// A batch can straddle the swap. Its last accepted delta still has to
// render, and an old done must not stop the new session's running turn.
func TestSessionEventsKeepCurrentBatchAndRemount(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	old := cfgWith(t, nil, nil, fakeHist{path: filepath.Join(dir, "old.jsonl")})
	current := cfgWith(t, nil, nil, fakeHist{path: filepath.Join(dir, "current.jsonl")})
	d := newDrv(t, 100, 30, current)
	d.typeStr("new question")
	d.feed(keyEnter())
	d.feed(eventsMsg{
		old.eventOf(Event{Kind: "assistant-delta", Text: "stale prefix"}),
		current.eventOf(Event{Kind: "assistant-delta", Text: "fresh answer"}),
		old.eventOf(Event{Kind: "assistant-delta", Text: "stale suffix"}),
		old.eventOf(Event{Kind: "done"}),
	})
	if s := d.plain(); !strings.Contains(s, "fresh answer") || strings.Contains(s, "stale") || !d.m.running {
		t.Fatalf("mixed batch lost or polluted the current turn (running=%v):\n%s", d.m.running, s)
	}
	// A theme/config remount in the same session does not discard events
	// from the previous subscription. Unscoped UI notices still render.
	d.cfgp.Store(cfgWith(t, nil, nil, current.hist))
	d.feed(eventsMsg{
		current.eventOf(Event{Kind: "assistant", Text: "completed answer"}),
		current.eventOf(Event{Kind: "done"}),
		{Kind: "system", Text: "global notice"},
	})
	if s := d.plain(); !strings.Contains(s, "completed answer") || !strings.Contains(s, "global notice") || d.m.running {
		t.Fatalf("same-session remount lost its events (running=%v):\n%s", d.m.running, s)
	}
}
