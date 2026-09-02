// Package uitest mounts a miniature but real bough tree — kernel +
// the plugins under test — plus the real ui transcript model, and
// drives the model in-process: no PTY, no browser, no tui goroutine.
// It pins the seam between a real plugin's output and the renderer.
//
// Every Driver is self-contained (own kernel Context, own model, own
// channels), so tests using it are parallel-safe as long as the plugins
// they mount touch only per-test paths.
package uitest

import (
	"context"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/ui"
)

const waitTimeout = 4 * time.Second

// LLMFunc adapts a pure func to the "llm" service for deterministic
// stub providers. It is the test's parrot: no network, ever.
type LLMFunc func(system string, messages []llm.Message) string

func (f LLMFunc) Complete(_ context.Context, system string, messages []llm.Message) (string, error) {
	return f(system, messages), nil
}

// LastUser returns the last user message's content (what llm-echo keys on).
func LastUser(messages []llm.Message) string {
	last := ""
	for _, m := range messages {
		if m.Role == "user" {
			last = m.Content
		}
	}
	return last
}

// Driver drives the real ui model against a mounted kernel tree.
// All methods must be called from the test goroutine.
type Driver struct {
	t     *testing.T
	Ctx   *kernel.Context
	model tea.Model
	msgs  chan tea.Msg
	done  chan struct{}
	quit  bool
}

// Mount builds a fresh kernel Context, runs pre (nil ok) to pre-provide
// stub services, mounts one row per plugin name (id = plugin), wires
// the real ui model to it, and returns the Driver. Cleanup (unmount,
// stop pumping) is registered on t.
func Mount(t *testing.T, pre func(*kernel.Context), plugins ...string) *Driver {
	t.Helper()
	ctx := kernel.NewContext()
	if pre != nil {
		pre(ctx)
	}
	rows := make([]kernel.Row, len(plugins))
	for i, p := range plugins {
		rows[i] = kernel.Row{ID: p, Plugin: p}
	}
	if err := ctx.Mount(rows); err != nil {
		t.Fatalf("uitest: mount %v: %v", plugins, err)
	}
	t.Cleanup(ctx.Unmount)
	m, err := ui.NewTestModel(ctx, 100, 40)
	if err != nil {
		t.Fatalf("uitest: model: %v", err)
	}
	d := &Driver{t: t, Ctx: ctx, model: m, msgs: make(chan tea.Msg, 64), done: make(chan struct{})}
	t.Cleanup(func() { close(d.done) }) // runs before Unmount (LIFO)
	d.exec(d.model.Init())
	return d
}

// exec runs a tea.Cmd like the real runtime: in a goroutine, its Msg
// posted back to the pump. Batches fan out.
func (d *Driver) exec(cmd tea.Cmd) {
	if cmd == nil {
		return
	}
	go func() {
		d.post(cmd())
	}()
}

func (d *Driver) post(msg tea.Msg) {
	if msg == nil {
		return
	}
	if batch, ok := msg.(tea.BatchMsg); ok {
		for _, c := range batch {
			d.exec(c)
		}
		return
	}
	select {
	case d.msgs <- msg:
	case <-d.done:
	}
}

// deliver feeds one Msg through Update on the test goroutine.
func (d *Driver) deliver(msg tea.Msg) {
	if _, ok := msg.(tea.QuitMsg); ok {
		d.quit = true
		return
	}
	m, cmd := d.model.Update(msg)
	d.model = m
	d.exec(cmd)
}

// Type sends s rune-by-rune as real key presses through the model's
// key handling (composer, keymap service, the works).
func (d *Driver) Type(s string) {
	for _, r := range s {
		d.deliver(tea.KeyPressMsg{Code: r, Text: string(r)})
	}
}

// Press sends named keys: "enter", "tab", "up", "ctrl+o", ...
func (d *Driver) Press(keys ...string) {
	for _, k := range keys {
		d.deliver(keyMsg(k))
	}
}

// Say types a line and presses enter — one user turn.
func (d *Driver) Say(line string) {
	d.Type(line)
	d.Press("enter")
}

// Step drains every immediately-available pending Msg without blocking.
func (d *Driver) Step() {
	for {
		select {
		case msg := <-d.msgs:
			d.deliver(msg)
		default:
			return
		}
	}
}

// Frame returns the current rendered frame, ANSI-stripped.
func (d *Driver) Frame() string { return ansi.Strip(d.RawFrame()) }

// RawFrame returns the current rendered frame with styling intact.
func (d *Driver) RawFrame() string { return d.model.View().Content }

// WaitFor pumps events until the rendered frame contains substr,
// failing the test with the full frame on timeout.
func (d *Driver) WaitFor(substr string) {
	d.t.Helper()
	d.wait(func() bool { return strings.Contains(d.Frame(), substr) }, "frame to contain "+substr)
}

// WaitQuit pumps until the model requested tea.Quit.
func (d *Driver) WaitQuit() {
	d.t.Helper()
	d.wait(func() bool { return d.quit }, "quit")
}

func (d *Driver) wait(pred func() bool, what string) {
	d.t.Helper()
	deadline := time.After(waitTimeout)
	for {
		if pred() {
			return
		}
		select {
		case msg := <-d.msgs:
			d.deliver(msg)
		case <-deadline:
			d.t.Fatalf("uitest: timed out waiting for %s\nframe:\n%s", what, d.Frame())
		}
	}
}

// keyMsg parses "enter", "ctrl+q", "pgup", "a" into a key press.
var specialKeys = map[string]rune{
	"enter":  tea.KeyEnter,
	"tab":    tea.KeyTab,
	"esc":    tea.KeyEscape,
	"up":     tea.KeyUp,
	"down":   tea.KeyDown,
	"pgup":   tea.KeyPgUp,
	"pgdown": tea.KeyPgDown,
	"space":  ' ',
}

func keyMsg(s string) tea.KeyPressMsg {
	var mod tea.KeyMod
	for {
		i := strings.IndexByte(s, '+')
		if i < 0 {
			break
		}
		switch s[:i] {
		case "ctrl":
			mod |= tea.ModCtrl
		case "alt":
			mod |= tea.ModAlt
		case "shift":
			mod |= tea.ModShift
		}
		s = s[i+1:]
	}
	if r, ok := specialKeys[s]; ok {
		return tea.KeyPressMsg{Code: r, Mod: mod}
	}
	k := tea.KeyPressMsg{Code: []rune(s)[0], Mod: mod}
	if mod == 0 {
		k.Text = s
	}
	return k
}
