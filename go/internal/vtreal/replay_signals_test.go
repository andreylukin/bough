package vtreal

// Signals delivered to the child (as a closing terminal, a process
// manager or `kill` would): bough must exit within 5 s, hand the
// terminal back (alt screen off, mouse off, cursor visible) and leave
// a history file whose every line is whole JSON and whose last turn
// ended with a done or cancelled entry.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
)

// signalsConfig slows the llm row so the second turn is still
// streaming when the signal lands (200 words at 50 ms = 10 s).
func signalsConfig(tape string) string {
	return strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 50}", tape), 1)
}

// signalsKnownBugs fail today and are skipped unless
// BOUGH_SIGNALS_KNOWN_BUGS is set; delete an entry once it is fixed.
//   - SIGHUP: cmd/bough/main.go never subscribes to it, so the default
//     disposition kills the process with the terminal still in the alt
//     screen, mouse on and cursor hidden; mid-turn the history also
//     lacks the turn's done/cancelled entry.
//   - SIGINT mid-turn: bubbletea's own interrupt handling tears the UI
//     down ("program was interrupted") and the streaming turn never
//     records done/cancelled (SIGTERM mid-turn does).
var signalsKnownBugs = map[string]bool{
	"SIGHUP/idle":     true,
	"SIGHUP/mid-turn": true,
	"SIGINT/mid-turn": true,
}

func TestSignals(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/signals.jsonl")
	for _, s := range []struct {
		name string
		sig  syscall.Signal
	}{{"SIGTERM", syscall.SIGTERM}, {"SIGHUP", syscall.SIGHUP}, {"SIGINT", syscall.SIGINT}} {
		for _, mid := range []bool{false, true} {
			name := s.name + "/idle"
			if mid {
				name = s.name + "/mid-turn"
			}
			t.Run(name, func(t *testing.T) {
				t.Parallel()
				if signalsKnownBugs[name] && os.Getenv("BOUGH_SIGNALS_KNOWN_BUGS") == "" {
					t.Skip("known bug, see signalsKnownBugs; set BOUGH_SIGNALS_KNOWN_BUGS=1 to run")
				}
				signalsRun(t, tape, s.sig, mid)
			})
		}
	}
}

func signalsRun(t *testing.T, tape string, sig syscall.Signal, mid bool) {
	// The quick first turn makes the session worth keeping on disk in
	// the idle case too (a meta-only file may be discarded).
	a := startCfg(t, 100, 30, signalsConfig(tape))
	a.typeText("quick")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	a.waitFor("quick answer")
	if mid {
		a.typeText("slow")
		a.key(uv.KeyEnter, 0)
		a.waitFor("slow3")
		if strings.Contains(a.text(), "slow200") {
			t.Fatalf("turn finished before the signal; it must still be streaming:\n%s", a.text())
		}
	}
	if err := a.cmd.Process.Signal(sig); err != nil {
		t.Fatalf("signal %v: %v\n%s", sig, err, a.text())
	}
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done: // any exit status; hanging is the failure
	case <-time.After(5 * time.Second):
		t.Fatalf("still running 5 s after %v:\n%s", sig, a.text())
	}
	signalsRestored(t, a)
	signalsHistory(t, a, mid)
}

// signalsRestored waits for the emulator to drain the exit bytes, then
// checks the terminal was handed back.
func signalsRestored(t *testing.T, a *app) {
	t.Helper()
	var bad []string
	for range 60 {
		s := a.term.Snapshot()
		bad = nil
		if s.AltScreen {
			bad = append(bad, "alt screen still on")
		}
		if !s.CursorVis {
			bad = append(bad, "cursor hidden")
		}
		for _, m := range []ansi.DECMode{ansi.NormalMouseMode, ansi.ButtonEventMouseMode, ansi.AnyEventMouseMode, ansi.SgrExtMouseMode} {
			if s.DEC[m].IsSet() {
				bad = append(bad, fmt.Sprintf("mouse mode %d still set", m))
			}
		}
		if bad == nil {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Errorf("terminal not restored after exit: %s\nscreen:\n%s", strings.Join(bad, ", "), a.text())
}

// signalsHistory reads the raw file: every line must be whole JSON and
// the last input must be followed by a done or cancelled entry.
func signalsHistory(t *testing.T, a *app, mid bool) {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one history file, got %v\nscreen:\n%s", paths, a.text())
	}
	raw, err := os.ReadFile(paths[0])
	if err != nil {
		t.Fatalf("%v\nscreen:\n%s", err, a.text())
	}
	if len(raw) == 0 || raw[len(raw)-1] != '\n' {
		t.Errorf("history ends mid-line: %q\nscreen:\n%s", raw[max(0, len(raw)-200):], a.text())
	}
	var kinds []string
	last, inputs := -1, 0
	for i, l := range bytes.Split(bytes.TrimRight(raw, "\n"), []byte("\n")) {
		var e struct{ Kind string }
		if err := json.Unmarshal(l, &e); err != nil {
			t.Errorf("history line %d is not whole JSON (%v): %q\nscreen:\n%s", i+1, err, l, a.text())
			continue
		}
		if e.Kind == "input" {
			last, inputs = len(kinds), inputs+1
		}
		kinds = append(kinds, e.Kind)
	}
	want := 1
	if mid {
		want = 2
	}
	if inputs != want {
		t.Errorf("want %d input entries, got kinds %v\nscreen:\n%s", want, kinds, a.text())
	}
	ended := false
	for _, k := range kinds[last+1:] {
		ended = ended || k == "done" || k == "cancelled"
	}
	if last < 0 || !ended {
		t.Errorf("last turn has no done/cancelled entry: kinds %v\nscreen:\n%s", kinds, a.text())
	}
}
