package ui

// Test driver: constructs the real model and feeds messages straight
// through Update/View. teatest/v2 (charm.land-compatible) is used for
// the full-program tests in teatest_test.go; everything else drives
// the model directly for speed and determinism.

import (
	"regexp"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// fakeHist implements historyView for status-bar/inspector tests.
type fakeHist struct {
	entries []history.Entry
	path    string
}

func (f fakeHist) Entries() []history.Entry { return f.entries }
func (f fakeHist) Path() string             { return f.path }

func histWith(path string, texts ...string) fakeHist {
	at := time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC)
	h := fakeHist{path: path}
	for i, txt := range texts {
		h.entries = append(h.entries, history.Entry{
			Seq: int64(i + 1), At: at, Kind: "user",
			Data: map[string]any{"text": txt},
		})
	}
	return h
}

// cfgWith builds a uiCfg with optional theme/keymap overrides and history.
func cfgWith(t *testing.T, themeOv, keymapOv map[string]string, hist historyView) *uiCfg {
	t.Helper()
	th := defaultTheme()
	if themeOv != nil {
		if err := th.apply(themeOv); err != nil {
			t.Fatalf("theme override: %v", err)
		}
	}
	keys := defaultKeymap()
	if keymapOv != nil {
		if err := applyKeymap(keys, keymapOv); err != nil {
			t.Fatalf("keymap override: %v", err)
		}
	}
	return newCfg(th, keys, "bough", hist)
}

// drv drives one model instance.
type drv struct {
	t    *testing.T
	m    model
	cfgp *atomic.Pointer[uiCfg]
	sent []string
}

func newDrv(t *testing.T, w, h int, cfg *uiCfg) *drv {
	t.Helper()
	d := &drv{t: t}
	var p atomic.Pointer[uiCfg]
	p.Store(cfg)
	d.cfgp = &p
	d.m = newModel(w, h, func(line string) { d.sent = append(d.sent, line) }, nil, &p)
	return d
}

func defaultDrv(t *testing.T) *drv {
	return newDrv(t, 80, 24, cfgWith(t, nil, nil, nil))
}

// feed runs one msg through Update without executing returned commands
// (loop-event handling returns a blocking waitEvent cmd).
func (d *drv) feed(msg tea.Msg) {
	next, _ := d.m.Update(msg)
	d.m = next.(model)
}

// press runs a key through Update and executes the returned commands
// one level deep (expanding tea.BatchMsg), feeding produced messages
// back in. Returns every message the commands produced, so callers can
// look for tea.QuitMsg.
func (d *drv) press(k tea.KeyPressMsg) []tea.Msg {
	next, cmd := d.m.Update(k)
	d.m = next.(model)
	msgs := runCmd(cmd)
	for _, m := range msgs {
		if _, quit := m.(tea.QuitMsg); quit {
			continue
		}
		d.feed(m)
	}
	return msgs
}

func runCmd(cmd tea.Cmd) []tea.Msg {
	if cmd == nil {
		return nil
	}
	msg := cmd()
	if batch, ok := msg.(tea.BatchMsg); ok {
		var out []tea.Msg
		for _, c := range batch {
			if c == nil {
				continue
			}
			if m := c(); m != nil {
				out = append(out, m)
			}
		}
		return out
	}
	if msg == nil {
		return nil
	}
	return []tea.Msg{msg}
}

// event feeds one loop event (the tea-loop path, minus the blocking
// wait for the next one).
func (d *drv) event(kind, text string) {
	d.feed(eventMsg{Kind: kind, Text: text})
}

// typeStr types printable text into the composer. It feeds without
// executing returned commands: textinput schedules a ~500ms cursor
// blink per keystroke, which press() would block on.
func (d *drv) typeStr(s string) {
	for _, r := range s {
		d.feed(tea.KeyPressMsg{Code: r, Text: string(r)})
	}
}

// view returns the rendered frame (with ANSI), plain the same stripped.
func (d *drv) view() string  { return d.m.View().Content }
func (d *drv) plain() string { return stripANSI(d.view()) }

var ansiRe = regexp.MustCompile(`\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07`)

func stripANSI(s string) string { return ansiRe.ReplaceAllString(s, "") }

func windowSize(w, h int) tea.WindowSizeMsg { return tea.WindowSizeMsg{Width: w, Height: h} }

// Key constructors.
func keyEnter() tea.KeyPressMsg  { return tea.KeyPressMsg{Code: tea.KeyEnter} }
func keyTab() tea.KeyPressMsg    { return tea.KeyPressMsg{Code: tea.KeyTab} }
func keyUp() tea.KeyPressMsg     { return tea.KeyPressMsg{Code: tea.KeyUp} }
func keyDown() tea.KeyPressMsg   { return tea.KeyPressMsg{Code: tea.KeyDown} }
func keyPgUp() tea.KeyPressMsg   { return tea.KeyPressMsg{Code: tea.KeyPgUp} }
func keyPgDown() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyPgDown} }
func keyCtrl(r rune) tea.KeyPressMsg {
	return tea.KeyPressMsg{Code: r, Mod: tea.ModCtrl}
}

func hasQuit(msgs []tea.Msg) bool {
	for _, m := range msgs {
		if _, ok := m.(tea.QuitMsg); ok {
			return true
		}
	}
	return false
}

// nLines builds "l1\nl2\n...\nlN".
func nLines(n int) string {
	parts := make([]string, n)
	for i := range parts {
		parts[i] = "l" + string(rune('0'+i%10)) + strings.Repeat("x", 3)
	}
	return strings.Join(parts, "\n")
}

func spinnerFrameIn(s string) bool {
	for _, f := range []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"} {
		if strings.Contains(s, f) {
			return true
		}
	}
	return false
}
