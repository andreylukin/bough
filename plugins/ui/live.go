package ui

import (
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// Live wiring shared by the tui and web modes across row remounts.
// The terminal (and the web listen address) can only be owned once
// per process, but hot reload remounts the ui row with a fresh inputs
// channel — so the running programs read events from a process-level
// broadcaster and send input through a swappable channel, and each
// mount just re-points both (same pattern as headless.go).

// historyView is the slice of the "history" service the UI reads.
// (*history.Store satisfies it; Append stays the loop's business.)
type historyView interface {
	Entries() []history.Entry
	Path() string
}

// uiCfg is one mount's immutable UI configuration; models read the
// current one through liveCfg on every render, so a remount (new
// theme/keymap row, history landing) restyles running views.
type uiCfg struct {
	theme   theme
	keys    map[string]string // action -> key
	action  map[string]string // key -> action (derived)
	status   string      // status-bar left text
	hist     historyView // nil when no history service
	mdStyle  string      // "dark"/"light" glamour override; "" = detect
	collapse string      // "all" | "large" | "none": which code/result blocks start collapsed
}

var (
	liveMu     sync.Mutex
	liveInputs chan<- string // current mount's inputs; nil while unmounted
	liveB      = &broadcaster{subs: map[int]chan Event{}}
	liveCfg    atomic.Pointer[uiCfg]
)

func init() {
	liveCfg.Store(newCfg(defaultTheme(), defaultKeymap(), "bough", nil))
}

func newCfg(t theme, keys map[string]string, status string, hist historyView) *uiCfg {
	action := make(map[string]string, len(keys))
	for a, k := range keys {
		if k != "" { // unbound (config-only) actions
			action[k] = a
		}
	}
	return &uiCfg{theme: t, keys: keys, action: action, status: status, hist: hist, collapse: "all"}
}

// buildCfg reads the optional "theme", "keymap" and "history" services
// (Get during Apply registers them as live dependencies: providing one
// later remounts the ui row) plus the row table for the status bar.
// rowCfg is the ui row's config: collapse: "all" (default) | "large" |
// "none" picks which code/result blocks start collapsed.
func buildCfg(ctx *kernel.Context, rowCfg map[string]any) (*uiCfg, error) {
	t := defaultTheme()
	mdStyle := ""
	if m, ok, err := getStringMap(ctx, "theme"); err != nil {
		return nil, err
	} else if ok {
		// "markdown" is not a style token: it overrides the glamour
		// style ("dark"/"light") picked from the detected background.
		if v, has := m["markdown"]; has {
			if v != "dark" && v != "light" {
				return nil, fmt.Errorf("ui: theme: markdown must be \"dark\" or \"light\", got %q", v)
			}
			mdStyle = v
			m2 := make(map[string]string, len(m))
			for k, vv := range m {
				if k != "markdown" {
					m2[k] = vv
				}
			}
			m = m2
		}
		if err := t.apply(m); err != nil {
			return nil, err
		}
	}
	keys := defaultKeymap()
	if m, ok, err := getStringMap(ctx, "keymap"); err != nil {
		return nil, err
	} else if ok {
		if err := applyKeymap(keys, m); err != nil {
			return nil, err
		}
	}
	var hist historyView
	if h, err := kernel.Get[historyView](ctx, "history"); err == nil {
		hist = h
	}

	status := "bough"
	rows := ctx.Rows()
	provider := ""
	for _, r := range rows {
		if r.ID == "llm" || strings.HasPrefix(r.Plugin, "llm-") {
			provider = r.Plugin
		}
	}
	if provider != "" {
		status += " · " + provider
	}
	status += fmt.Sprintf(" · %d rows", len(rows))
	cfg := newCfg(t, keys, status, hist)
	cfg.mdStyle = mdStyle
	if v, has := rowCfg["collapse"]; has {
		s, ok := v.(string)
		if !ok || (s != "all" && s != "large" && s != "none") {
			return nil, fmt.Errorf("ui: collapse must be \"all\", \"large\" or \"none\", got %v", v)
		}
		cfg.collapse = s
	}
	return cfg, nil
}

// getStringMap fetches a map[string]string service, tolerating the
// map[string]any shape a JS-provided config arrives as. Absent is
// fine (ok=false); present with a non-string shape fails loud.
func getStringMap(ctx *kernel.Context, key string) (map[string]string, bool, error) {
	v, err := kernel.Get[any](ctx, key)
	if err != nil {
		return nil, false, nil // absent
	}
	switch m := v.(type) {
	case map[string]string:
		return m, true, nil
	case map[string]any:
		out := make(map[string]string, len(m))
		for k, av := range m {
			s, ok := av.(string)
			if !ok {
				return nil, false, fmt.Errorf("ui: service %q: value of %q is %T, not string", key, k, av)
			}
			out[k] = s
		}
		return out, true, nil
	default:
		return nil, false, fmt.Errorf("ui: service %q is %T, not map[string]string", key, v)
	}
}

// attachLive points the live wiring at this mount: swap the config
// and inputs, bridge kernel loop/events into the process broadcaster
// (the On subscription is auto-disposed with the row), and detach
// inputs on unmount so nothing sends into a closed channel.
func attachLive(ctx *kernel.Context, inputs chan<- string, cfg *uiCfg) {
	liveCfg.Store(cfg)
	liveMu.Lock()
	liveInputs = inputs
	liveMu.Unlock()
	ctx.On("loop/event", func(payload any) {
		liveB.publish(eventOf(payload))
	})
	ctx.Effect(func() {
		liveMu.Lock()
		if liveInputs == inputs {
			liveInputs = nil
		}
		liveMu.Unlock()
	})
}

// sendLive delivers one input line to the current mount, waiting out
// a remount window (sending under the lock, like headless: the row
// disposer blocks until an in-flight send finishes). After ~5s with
// no loop attached the line is dropped loudly.
func sendLive(line string) {
	for i := 0; i < 100; i++ {
		liveMu.Lock()
		ch := liveInputs
		if ch != nil {
			ch <- line
			liveMu.Unlock()
			return
		}
		liveMu.Unlock()
		time.Sleep(50 * time.Millisecond)
	}
	fmt.Fprintln(os.Stderr, "ui: input dropped: no loop attached after 5s")
}
