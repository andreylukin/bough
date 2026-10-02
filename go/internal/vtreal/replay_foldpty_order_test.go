package vtreal

import (
	"fmt"
	"io"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/vt"
)

// Model an input queue whose Tab and Enter paints each arrive after
// settled's quiet window. A late Tab frame must not acknowledge Enter,
// and a valid no-op (the only focus stop) still needs an acknowledgement.
func TestFoldPtyKeyFrameWaitsForInput(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		initial string
		states  []string
		keys    []rune
	}{
		{"delayed tab and toggles", "> ▸ older", []string{"> ▸ result", "> ▾ result\r\n  body", "> ▸ result", "> ▸ result"}, []rune{uv.KeyTab, uv.KeyEnter, uv.KeyEnter, uv.KeyTab}},
		{"no focusable blocks", "reply\r\n> say something", []string{"reply\r\n> say something", "reply\r\n> say something"}, []rune{uv.KeyTab, uv.KeyEnter}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				term := &Terminal{Emu: vt.NewSafeEmulator(40, 5), cols: 40, rows: 5}
				defer term.Emu.Close()
				a := &app{t: t, term: term, cols: 40, rows: 5}
				w := stampWriter{term.Emu, &term.lastOut}
				paint := func(s string) { fmt.Fprintf(w, "\x1b[2J\x1b[H%s", s) }
				paint(tc.initial)
				states, keys := tc.states, tc.keys
				// Like the PTY, input accepts the whole sequence while the app is
				// busy. Reading directly in the painter would accidentally make
				// SendKey itself the acknowledgement this test is meant to prove.
				input := make(chan byte, len(keys)*7)
				readDone := make(chan struct{})
				go func() {
					defer close(readDone)
					for range len(keys) * 7 { // key, ctrl+x, and the five-byte CSI-u Escape
						var b [1]byte
						if _, err := io.ReadFull(term.Emu, b[:]); err != nil {
							t.Error(err)
							return
						}
						input <- b[0]
					}
				}()
				painted := make(chan struct{})
				go func() {
					defer close(painted)
					for _, state := range states {
						<-input
						time.Sleep(250 * time.Millisecond)
						paint(state)
						if k := <-input; k != 0x18 {
							t.Errorf("leader byte = %x", k)
						}
						time.Sleep(250 * time.Millisecond)
						paint(state + "\x1b[4;1Hctrl+x …")
						var esc strings.Builder
						for range 5 {
							esc.WriteByte(<-input)
						}
						if esc.String() != "\x1b[27u" {
							t.Errorf("escape = %q", esc.String())
						}
						time.Sleep(250 * time.Millisecond)
						paint(state)
					}
				}()
				for i, key := range keys {
					got := foldPtyKeyFrame(a, key)
					want := strings.ReplaceAll(states[i], "\r", "")
					if strings.TrimRight(got, "\n") != want {
						t.Errorf("key %d returned an unacknowledged frame:\n%s\nwant:\n%s", i, got, want)
					}
				}
				<-readDone
				<-painted
			})
		})
	}
}

// The acknowledgement must leave the result's three-state cycle,
// keyboard focus and viewport intact, not only the two-state folds.
func TestFoldPtyKeyFrameTailWindowRoundTrip(t *testing.T) {
	t.Parallel()
	tape := streamPtyTape(t, []streamPtyStep{
		{reply: "```js\nconsole.log(1)\n```", code: "console.log(1)\n", result: strings.Repeat("a result line\n", 40)},
		streamPtyStop("finished"),
	})
	a := startCfg(t, 100, 60, replayConfig(tape))
	streamPtySend(a, "go")
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("finished")
	before := foldPtyKeyFrame(a, uv.KeyTab)
	if !strings.Contains(before, "> ▸ result") {
		t.Fatalf("newest result not focused:\n%s", before)
	}
	opened := foldPtyKeyFrame(a, uv.KeyEnter)
	if !strings.Contains(opened, "enter to view all") {
		t.Fatalf("first enter did not show tail window:\n%s", opened)
	}
	full := foldPtyKeyFrame(a, uv.KeyEnter)
	if strings.Contains(full, "enter to view all") || !strings.Contains(full, "> ▾ result") {
		t.Fatalf("second enter did not show full result:\n%s", full)
	}
	after := foldPtyKeyFrame(a, uv.KeyEnter)
	if after != before {
		t.Errorf("three acknowledged enters changed the initial frame:\nbefore:\n%s\nafter:\n%s", before, after)
	}
}
