package ui

// Full-program tests through teatest/v2 (charm.land/bubbletea/v2
// compatible): the real tea.Program with the model's event-channel
// wiring, typed input, and quit path.

import (
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/exp/teatest/v2"
)

// startProgram runs the model under teatest with a fake loop: send
// pushes the input line to sendFn, and events flow through the channel
// exactly as the live broadcaster would deliver them.
// sendQuit presses the quit key twice: one press only arms the quit
// (see stop.go), the second within the window quits.
func sendQuit(tm *teatest.TestModel) {
	tm.Send(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	tm.Send(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
}

func startProgram(t *testing.T, events chan Event, sendFn func(string)) *teatest.TestModel {
	t.Helper()
	t.Cleanup(func() { close(events) }) // unblock the final waitEvent
	var cfg atomic.Pointer[uiCfg]
	cfg.Store(newCfg(defaultTheme(), defaultKeymap(), "bough", nil))
	m := newModel(80, 24, sendFn, events, &cfg)
	return teatest.NewTestModel(t, m, teatest.WithInitialTermSize(80, 24))
}

// waitForOutput waits until the program's cumulative NEW output (each
// WaitFor drains the reader, so already-matched bytes are gone) contains
// every wanted substring, matching on ANSI-stripped bytes because the
// diff renderer interleaves cursor movement with text.
func waitForOutput(t *testing.T, tm *teatest.TestModel, wants ...string) {
	t.Helper()
	teatest.WaitFor(t, tm.Output(), func(b []byte) bool {
		plain := stripANSI(string(b))
		for _, w := range wants {
			if !strings.Contains(plain, w) {
				return false
			}
		}
		return true
	}, teatest.WithDuration(4*time.Second), teatest.WithCheckInterval(10*time.Millisecond))
}

func TestProgramTurnFlow(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	send := func(line string) { // the fake loop echoes deterministically
		events <- Event{Kind: "assistant", Text: "echo: " + line}
		events <- Event{Kind: "done"}
	}
	tm := startProgram(t, events, send)

	tm.Type("hello world")
	tm.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	// user block + deterministic echo reply, in one wait (each WaitFor
	// drains the output reader).
	waitForOutput(t, tm, "❯ hello world", "echo: hello world")

	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
}

func TestProgramSpinnerBetweenSendAndDone(t *testing.T) {
	t.Parallel()
	gate := make(chan struct{})
	events := make(chan Event, 16)
	send := func(line string) {
		<-gate // hold the turn open so the spinner is observable
		events <- Event{Kind: "assistant", Text: "late"}
		events <- Event{Kind: "done"}
	}
	tm := startProgram(t, events, send)

	tm.Type("slow one")
	tm.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	teatest.WaitFor(t, tm.Output(), func(b []byte) bool {
		return spinnerFrameIn(string(b))
	}, teatest.WithDuration(4*time.Second), teatest.WithCheckInterval(10*time.Millisecond))

	close(gate)
	waitForOutput(t, tm, "late")
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
}

func TestProgramExternalEventsRender(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	tm := startProgram(t, events, func(string) {})
	events <- Event{Kind: "code", Text: `tools.bash("echo hi from codemode")`}
	events <- Event{Kind: "result", Text: "hi from codemode"}
	events <- Event{Kind: "done"}
	waitForOutput(t, tm, "hi from codemode", "Ran: echo hi from codemode (1 line)")
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
}

func TestProgramMouseClickTogglesBlock(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	tm := startProgram(t, events, func(string) {})
	long := strings.TrimSuffix(strings.Repeat("BODYLINE\n", 20), "\n")
	events <- Event{Kind: "result", Text: long}
	// The collapsed 20-line result is the only block: its header is
	// the first transcript row.
	waitForOutput(t, tm, "▸ result (20 lines)")
	tm.Send(tea.MouseClickMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	tm.Send(tea.MouseReleaseMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	// Expanded: the body is visible and the ▾ header stays on row 0
	// (expanding keeps the clicked header on screen).
	waitForOutput(t, tm, "▾ result", "│ BODYLINE")
	// Clicking the header again collapses it. The renderer only
	// rewrites the changed glyph, so judge by the final model.
	tm.Send(tea.MouseClickMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	tm.Send(tea.MouseReleaseMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	fm, ok := tm.FinalModel(t).(model)
	if !ok || len(fm.blocks) != 1 || !fm.blocks[0].collapsed {
		t.Fatalf("second click did not collapse the block (ok=%v blocks=%d)", ok, len(fm.blocks))
	}
}

func TestProgramQuitCleanly(t *testing.T) {
	t.Parallel()
	tm := startProgram(t, make(chan Event, 16), func(string) {})
	waitForOutput(t, tm, "bough") // first frame drawn
	sendQuit(tm)
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
	if fm, ok := tm.FinalModel(t).(model); !ok {
		t.Errorf("final model has unexpected type %T", tm.FinalModel(t))
	} else if fm.running {
		t.Error("quit with no turn in flight should not be running")
	}
}
