// Package ui is the UI plugin: one bubbletea model serving native TUI,
// web (sip), and a plain-stdio headless mode.
package ui

import (
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

// Event is a normalized loop/event payload.
type Event struct {
	Kind string
	Text string
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

	b := &broadcaster{subs: map[int]chan Event{}}
	dispose := ctx.On("loop/event", func(payload any) {
		b.publish(eventOf(payload))
	})
	ctx.Effect(dispose)

	switch {
	case mode == "headless":
		go runHeadless(inputs, b)
	case mode == "tui":
		go runTUI(inputs, b)
	case strings.HasPrefix(mode, "web:"):
		addr := strings.TrimPrefix(mode, "web:")
		return startWeb(ctx, addr, inputs, b)
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
type broadcaster struct {
	mu     sync.Mutex
	nextID int
	subs   map[int]chan Event
}

func (b *broadcaster) subscribe() (<-chan Event, func()) {
	b.mu.Lock()
	defer b.mu.Unlock()
	id := b.nextID
	b.nextID++
	ch := make(chan Event, 64)
	b.subs[id] = ch
	return ch, func() {
		b.mu.Lock()
		defer b.mu.Unlock()
		delete(b.subs, id)
	}
}

func (b *broadcaster) publish(ev Event) {
	b.mu.Lock()
	defer b.mu.Unlock()
	for _, ch := range b.subs {
		select {
		case ch <- ev:
		default: // slow subscriber: drop rather than block the emitter
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
			return Event{Kind: k.String(), Text: t.String()}
		}
	}
	return Event{Kind: "event", Text: fmt.Sprint(payload)}
}
