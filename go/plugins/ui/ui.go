// Package ui is the UI plugin: one bubbletea model serving native TUI,
// web (sip), and a plain-stdio headless mode.
package ui

import (
	"github.com/andreylukin/bough/plugins/llm"

	"fmt"
	"os"
	"reflect"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("ui", func() kernel.Plugin { return &plugin{} })
}

// Event is a normalized loop/event payload. ID and Options are only
// set for kind "ask" (the ask plugin's event carries them).
type Event struct {
	Kind    string
	Text    string
	ID      string
	Options []string
	Data    map[string]any // extra payload (e.g. done's files/exit); nil when absent
}

type plugin struct{}

func (p *plugin) Name() string     { return "ui" }
func (p *plugin) Inject() []string { return []string{"inputs"} }

func (p *plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	inputs, err := kernel.Get[chan string](ctx, "inputs")
	if err != nil {
		return err
	}
	mode, err := kernel.Get[string](ctx, "ui-mode")
	if err != nil {
		return err
	}

	if mode == "headless" {
		b := &broadcaster{subs: map[int]*subscriber{}}
		dispose := ctx.On("loop/event", func(payload any) {
			b.publish(eventOf(payload))
		})
		ctx.Effect(dispose)
		// Optional seams, like the tui/web modes: without a commands
		// service a "/" line is plain text; without history the
		// dispatches just aren't recorded.
		var cmds commandsView
		if c, err := kernel.Get[commandsView](ctx, "commands"); err == nil {
			cmds = c
		}
		var hlog historyAppender
		if a, err := kernel.Get[historyAppender](ctx, "history"); err == nil {
			hlog = a
		}
		var ask askAnswers
		if a, err := kernel.Get[askAnswers](ctx, "ask-answers"); err == nil {
			ask = a
		}
		var steer func(string) bool
		if s, err := kernel.Get[func(string) bool](ctx, "steer"); err == nil {
			steer = s
		}
		// With a usage service (cost row) every done line is followed by
		// a "[usage] {...}" JSON line: harness benchmarks read spend
		// from it.
		hlMu.Lock()
		hlUsage = nil
		if u, err := kernel.Get[llm.UsageReporter](ctx, "usage"); err == nil {
			hlUsage = u
		}
		hlMu.Unlock()
		ctx.Effect(runHeadless(inputs, b, cmds, hlog, ask, steer))
		return nil
	}

	// tui/web: themed, keymap-driven views on the process-level live
	// wiring (see live.go). A remount re-points config and inputs;
	// the running program/server is a process singleton.
	ucfg, err := buildCfg(ctx, cfg)
	if err != nil {
		return err
	}
	attachLive(ctx, inputs, ucfg)

	switch {
	case mode == "tui":
		runTUI()
	case strings.HasPrefix(mode, "web:"):
		return startWeb(strings.TrimPrefix(mode, "web:"))
	default:
		return fmt.Errorf("ui: unknown ui-mode %q", mode)
	}
	return nil
}

// interruptSelf signals the launcher (blocked on SIGINT/SIGTERM) so the
// process unmounts and exits 0.
func interruptSelf() {
	p, err := os.FindProcess(os.Getpid())
	if err != nil {
		os.Exit(0)
	}
	if err := p.Signal(os.Interrupt); err != nil {
		os.Exit(0)
	}
}

// broadcaster fans a single kernel subscription out to per-view channels.
// Each subscriber has its own unbounded queue drained by a goroutine, so
// publish never blocks the emitter and a slow view never loses events
// (a dropped final assistant/done left the spinner running forever).
type broadcaster struct {
	mu     sync.Mutex
	nextID int
	subs   map[int]*subscriber
}

type subscriber struct {
	mu   sync.Mutex
	q    []Event
	wake chan struct{} // cap 1: queue has events
	done chan struct{} // closed on unsubscribe
}

func (b *broadcaster) subscribe() (<-chan Event, func()) {
	b.mu.Lock()
	defer b.mu.Unlock()
	id := b.nextID
	b.nextID++
	s := &subscriber{wake: make(chan struct{}, 1), done: make(chan struct{})}
	ch := make(chan Event, 64)
	go s.drain(ch)
	b.subs[id] = s
	return ch, func() {
		b.mu.Lock()
		defer b.mu.Unlock()
		if _, ok := b.subs[id]; ok {
			delete(b.subs, id)
			close(s.done)
		}
	}
}

func (b *broadcaster) publish(ev Event) {
	b.mu.Lock()
	defer b.mu.Unlock()
	for _, s := range b.subs {
		s.mu.Lock()
		s.q = append(s.q, ev)
		s.mu.Unlock()
		select {
		case s.wake <- struct{}{}:
		default:
		}
	}
}

// drain forwards the queue to ch in order until unsubscribe.
func (s *subscriber) drain(ch chan<- Event) {
	for {
		select {
		case <-s.done:
			return
		case <-s.wake:
		}
		s.mu.Lock()
		q := s.q
		s.q = nil
		s.mu.Unlock()
		for _, ev := range q {
			select {
			case ch <- ev:
			case <-s.done:
				return
			}
		}
	}
}

// eventOf normalizes a loop/event payload ({Kind, Text} as struct, pointer,
// or map) into an Event.
func eventOf(payload any) Event {
	switch v := payload.(type) {
	case Event:
		return v
	case map[string]string:
		return Event{Kind: v["Kind"], Text: v["Text"]}
	case map[string]any:
		k, _ := v["Kind"].(string)
		t, _ := v["Text"].(string)
		return Event{Kind: k, Text: t}
	}
	rv := reflect.ValueOf(payload)
	if rv.Kind() == reflect.Pointer {
		rv = rv.Elem()
	}
	if rv.Kind() == reflect.Struct {
		k := rv.FieldByName("Kind")
		t := rv.FieldByName("Text")
		if k.IsValid() && t.IsValid() && k.Kind() == reflect.String && t.Kind() == reflect.String {
			ev := Event{Kind: k.String(), Text: t.String()}
			if f := rv.FieldByName("ID"); f.IsValid() && f.Kind() == reflect.String {
				ev.ID = f.String()
			}
			if f := rv.FieldByName("Options"); f.IsValid() && f.CanInterface() {
				if opts, ok := f.Interface().([]string); ok {
					ev.Options = opts
				}
			}
			if f := rv.FieldByName("Data"); f.IsValid() && f.CanInterface() {
				if d, ok := f.Interface().(map[string]any); ok {
					ev.Data = d
				}
			}
			return ev
		}
	}
	return Event{Kind: "event", Text: fmt.Sprint(payload)}
}
