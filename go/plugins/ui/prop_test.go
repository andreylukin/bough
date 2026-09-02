package ui

// Property tests: random sequences of keys, loop events, resizes,
// mouse and paste against the real model, with frame invariants
// checked after every step. rapid shrinks a failing sequence to the
// smallest one that still breaks an invariant.
//
//	go test ./plugins/ui -run Prop -rapid.checks=2000

import (
	"os"
	"strconv"
	"strings"
	"testing"
	"unicode"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"pgregory.net/rapid"
)

// propMinW is the narrowest terminal the properties consider (env
// BOUGH_PROP_MINW overrides, to triage which findings are narrow-only).
var propMinW = func() int {
	if n, err := strconv.Atoi(os.Getenv("BOUGH_PROP_MINW")); err == nil && n > 0 {
		return n
	}
	return 20
}()

// --- generators ---

var propKinds = []string{
	"assistant", "user", "error", "code", "result", "thinking", "system",
	"todo", "done", "cancelled", "spawn", "command", "bash", "patch",
}

// propWord is one "word": ascii, wide CJK, emoji, combining marks, or a
// very long unbroken run (URLs, hashes) that must wrap somewhere.
func propWord() *rapid.Generator[string] {
	return rapid.OneOf(
		rapid.StringMatching(`[a-zA-Z0-9_./:-]{1,12}`),
		rapid.SampledFrom([]string{"日本語", "中文字符", "한국어", "🐛", "👨‍👩‍👧", "é", "e\u0301", "→", "…", "▾", "▸", "❯"}),
		rapid.StringMatching(`[a-f0-9]{80,400}`),
		rapid.SampledFrom([]string{"\t", "\r", "\x1b[31mred\x1b[0m", "\x1b]8;;http://x\x07link\x1b]8;;\x07", "\x00", "\x7f"}),
	)
}

// propText is a body: words joined by spaces/newlines, sometimes
// wrapped in markdown structure the renderer must cope with.
func propText() *rapid.Generator[string] {
	return rapid.Custom(func(t *rapid.T) string {
		words := rapid.SliceOfN(propWord(), 0, 40).Draw(t, "words")
		var sb strings.Builder
		for i, w := range words {
			if i > 0 {
				sb.WriteString(rapid.SampledFrom([]string{" ", " ", "\n", "\n\n", ""}).Draw(t, "sep"))
			}
			sb.WriteString(w)
		}
		body := sb.String()
		switch rapid.IntRange(0, 6).Draw(t, "wrap") {
		case 0:
			return "```" + rapid.SampledFrom([]string{"", "js", "go", "sh"}).Draw(t, "lang") + "\n" + body + "\n```"
		case 1:
			return "```js\n" + body // unterminated fence
		case 2:
			return "| a | b |\n|---|---|\n| " + body + " | x |"
		case 3:
			return "# " + body
		case 4:
			return "- " + strings.ReplaceAll(body, "\n", "\n- ")
		}
		return body
	})
}

type propStep struct {
	kind string // key, event, resize, click, wheel, paste
	key  tea.KeyPressMsg
	ev   Event
	w, h int
	x, y int
	text string
}

var propKeys = []tea.KeyPressMsg{
	{Code: tea.KeyEnter}, {Code: tea.KeyTab}, {Code: tea.KeyTab, Mod: tea.ModShift},
	{Code: tea.KeyUp}, {Code: tea.KeyDown}, {Code: tea.KeyPgUp}, {Code: tea.KeyPgDown},
	{Code: tea.KeyHome}, {Code: tea.KeyEnd}, {Code: tea.KeyEscape}, {Code: tea.KeyBackspace},
	{Code: tea.KeyDelete}, {Code: tea.KeyLeft}, {Code: tea.KeyRight},
	{Code: tea.KeyEnter, Mod: tea.ModShift}, {Code: tea.KeyEnter, Mod: tea.ModAlt},
	{Code: 'o', Mod: tea.ModCtrl}, {Code: 'l', Mod: tea.ModCtrl}, {Code: 'a', Mod: tea.ModCtrl},
	{Code: 'e', Mod: tea.ModCtrl}, {Code: 'k', Mod: tea.ModCtrl}, {Code: 'u', Mod: tea.ModCtrl},
	{Code: 'w', Mod: tea.ModCtrl}, {Code: 'c', Mod: tea.ModCtrl},
}

func propStepGen(w, h int) *rapid.Generator[propStep] {
	return rapid.Custom(func(t *rapid.T) propStep {
		switch rapid.IntRange(0, 9).Draw(t, "kind") {
		case 0, 1, 2:
			return propStep{kind: "key", key: rapid.SampledFrom(propKeys).Draw(t, "key")}
		case 3, 4:
			r := rapid.OneOf(
				rapid.RuneFrom(nil, unicode.Letter, unicode.Digit, unicode.Punct, unicode.Symbol),
				rapid.SampledFrom([]rune{'/', '!', '?', '@', ' ', '日', '🐛'}),
			).Draw(t, "rune")
			return propStep{kind: "key", key: tea.KeyPressMsg{Code: r, Text: string(r)}}
		case 5, 6:
			ev := Event{Kind: rapid.SampledFrom(propKinds).Draw(t, "evkind"), Text: propText().Draw(t, "text")}
			if rapid.Bool().Draw(t, "ask") {
				ev.Kind = "ask"
				ev.ID = rapid.StringMatching(`[a-z]{3}`).Draw(t, "askid")
				ev.Options = rapid.SliceOfN(rapid.StringMatching(`[a-z ]{0,12}`), 0, 5).Draw(t, "opts")
			}
			return propStep{kind: "event", ev: ev}
		case 7:
			return propStep{kind: "resize",
				w: rapid.IntRange(propMinW, 200).Draw(t, "w"), h: rapid.IntRange(5, 60).Draw(t, "h")}
		case 8:
			if rapid.Bool().Draw(t, "wheel") {
				return propStep{kind: "wheel", x: rapid.IntRange(0, w).Draw(t, "x"), y: rapid.IntRange(0, h).Draw(t, "y"),
					w: rapid.IntRange(0, 1).Draw(t, "dir")}
			}
			return propStep{kind: "click", x: rapid.IntRange(0, w+2).Draw(t, "x"), y: rapid.IntRange(0, h+2).Draw(t, "y")}
		default:
			return propStep{kind: "paste", text: propText().Draw(t, "paste")}
		}
	})
}

// --- invariants ---

type propState struct {
	w, h     int
	lastCtrl bool // previous key was ctrl+c (quit is armed)
}

func (d *drv) apply(t *rapid.T, st *propState, s propStep, checkQuit bool) {
	switch s.kind {
	case "key":
		next, cmd := d.m.Update(s.key)
		d.m = next.(model)
		isCtrlC := s.key.Code == 'c' && s.key.Mod == tea.ModCtrl
		if isCtrlC && checkQuit {
			// The only path to tea.Quit; execute the cmd to see it.
			if hasQuit(runCmd(cmd)) && !st.lastCtrl {
				t.Fatalf("quit on a single ctrl+c (quit must be armed by a first press); picking=%v inspecting=%v", d.m.picking, d.m.inspecting)
			}
		}
		st.lastCtrl = isCtrlC
	case "event":
		d.feed(eventMsg(s.ev))
	case "resize":
		st.w, st.h = s.w, s.h
		d.feed(windowSize(s.w, s.h))
	case "click":
		d.feed(tea.MouseClickMsg{X: s.x, Y: s.y, Button: tea.MouseLeft})
		d.feed(tea.MouseReleaseMsg{X: s.x, Y: s.y, Button: tea.MouseLeft})
	case "wheel":
		b := tea.MouseWheelUp
		if s.w == 1 {
			b = tea.MouseWheelDown
		}
		d.feed(tea.MouseWheelMsg{X: s.x, Y: s.y, Button: b})
	case "paste":
		d.feed(tea.PasteMsg{Content: s.text})
		st.lastCtrl = false // typing (keys, paste) disarms; events, resizes and clicks do not
	}
}

// checkFrame asserts one frame invariant (by name) for the current state.
func checkFrame(t *rapid.T, d *drv, st *propState, which, step string) {
	raw := d.view()
	plain := d.plain()
	lines := strings.Split(raw, "\n")
	switch which {
	case "panic":
		if strings.Contains(plain, "✗ render failed") {
			t.Fatalf("after %s: render panicked:\n%s", step, plain)
		}
	case "height":
		if st.h >= 3 && len(lines) != st.h {
			t.Fatalf("after %s: frame is %d lines for a %dx%d terminal:\n%s", step, len(lines), st.w, st.h, plain)
		}
	case "width":
		for i, ln := range lines {
			if w := ansi.StringWidth(ln); w > st.w {
				t.Fatalf("after %s: line %d is %d cells wide in a %d-wide terminal:\n%q", step, i, w, st.w, ansi.Strip(ln))
			}
		}
	case "control":
		for i, ln := range lines {
			for _, r := range ansi.Strip(ln) {
				if r == '\t' || r == '\r' || r == 0 || r == 0x7f || (r < 0x20 && r != 0x1b) {
					t.Fatalf("after %s: line %d carries control char %q:\n%q", step, i, r, ansi.Strip(ln))
				}
			}
		}
	}
}

func TestPropFrameInvariants(t *testing.T) {
	t.Parallel()
	for _, which := range []string{"panic", "height", "width", "control", "quit"} {
		t.Run(which, func(t *testing.T) {
			t.Parallel()
			rapid.Check(t, func(rt *rapid.T) {
				st := &propState{w: rapid.IntRange(propMinW, 200).Draw(rt, "w0"), h: rapid.IntRange(5, 60).Draw(rt, "h0")}
				d := newDrv(t, st.w, st.h, cfgWith(t, nil, nil, histWith("/tmp/h.jsonl", "one", "two")))
				checkFrame(rt, d, st, which, "init")
				n := rapid.IntRange(1, 60).Draw(rt, "n")
				for i := 0; i < n; i++ {
					s := propStepGen(st.w, st.h).Draw(rt, "step")
					d.apply(rt, st, s, which == "quit")
					checkFrame(rt, d, st, which, s.kind)
				}
			})
		})
	}
}

// A typed draft is always visible: whatever printable text is in the
// composer shows in the frame (composer never clips its own content
// below composerMaxLines), and the draft survives resizes.
func TestPropDraftVisible(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		st := &propState{w: rapid.IntRange(propMinW, 200).Draw(rt, "w0"), h: rapid.IntRange(6, 60).Draw(rt, "h0")}
		d := newDrv(t, st.w, st.h, cfgWith(t, nil, nil, nil))
		word := rapid.StringMatching(`[a-z]{1,8}`).Draw(rt, "word")
		d.typeStr(word)
		for i := 0; i < rapid.IntRange(0, 5).Draw(rt, "resizes"); i++ {
			st.w, st.h = rapid.IntRange(20, 200).Draw(rt, "w"), rapid.IntRange(6, 60).Draw(rt, "h")
			d.feed(windowSize(st.w, st.h))
			checkFrame(rt, d, st, "width", "resize")
		}
		if !strings.Contains(d.plain(), word) {
			rt.Fatalf("draft %q lost from frame after resize to %dx%d:\n%s", word, st.w, st.h, d.plain())
		}
		if d.m.input.Value() != word {
			rt.Fatalf("composer value %q != typed %q", d.m.input.Value(), word)
		}
	})
}

// Block focus: k tabs from nothing-focused land on the (k-1)-th block
// older than the newest, and k-1 shift+tabs walk back to the newest.
func TestPropFocusCycle(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		d := newDrv(t, 100, 40, cfgWith(t, nil, nil, nil))
		n := rapid.IntRange(1, 8).Draw(rt, "blocks")
		for i := 0; i < n; i++ {
			d.event(rapid.SampledFrom([]string{"code", "result", "assistant", "bash"}).Draw(rt, "kind"), nLines(rapid.IntRange(1, 30).Draw(rt, "len")))
		}
		d.event("done", "")
		f := d.m.focusables()
		if len(f) == 0 {
			return
		}
		k := rapid.IntRange(1, 12).Draw(rt, "tabs")
		for i := 0; i < k; i++ {
			d.feed(keyTab())
		}
		want := d.m.blocks[f[((len(f)-1)-(k-1)%len(f)+len(f))%len(f)]].id
		if d.m.focusID != want {
			rt.Fatalf("after %d tabs over %d blocks: focus=%d want %d", k, len(f), d.m.focusID, want)
		}
		for i := 0; i < k-1; i++ {
			d.feed(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
		}
		if newest := d.m.blocks[f[len(f)-1]].id; d.m.focusID != newest {
			rt.Fatalf("tab x%d then shift+tab x%d did not return to newest: focus=%d want %d", k, k-1, d.m.focusID, newest)
		}
	})
}
