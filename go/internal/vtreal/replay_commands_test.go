package vtreal

// Slash commands in the middle of a replayed session. A command never
// reaches the model, so it must not consume a tape reply: after every
// command the next real input still gets the reply the recording had,
// and the tape never runs out ("[replay: end of tape]").
//
// The fixture tape also carries a "/help" input and a "command" entry,
// so TestCommandsTapeSkipsSlashInputs shows the driver skips "/" lines.

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

func commandsTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/commands.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// commandsSystemEntries is every "system" entry this run has written —
// where a command's output lands, whether or not it is still on screen
// (a long one, /context, scrolls away).
func commandsSystemEntries(a *app) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.Kind == "system" {
				text, _ := e.Data["text"].(string)
				out = append(out, text)
			}
		}
	}
	return out
}

// commandsDispatch types one "/..." line and waits for its output to
// reach the transcript log. Only entries written after the line was
// sent count, so an earlier command's output cannot satisfy it.
func commandsDispatch(t *testing.T, a *app, line, want string) {
	t.Helper()
	before := len(commandsSystemEntries(a))
	a.typeText(line)
	a.key(uv.KeyEnter, 0)
	var fresh string
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if all := commandsSystemEntries(a); len(all) > before {
			fresh = strings.Join(all[before:], "\n")
			if strings.Contains(fresh, want) {
				return
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("%s: output never mentioned %q\nsystem output since the command:\n%s\nscreen:\n%s",
		line, want, fresh, a.text())
}

// commandsCloseOverlay presses esc until the composer is back. A
// picker takes the whole pane, and in this harness its first esc is
// swallowed, so more than one press can be needed; the loop stops as
// soon as the composer shows, before a second esc on the chat view
// could start a rewind.
func commandsCloseOverlay(t *testing.T, a *app, what string) {
	t.Helper()
	for range 4 {
		if composerRow(a.lines()) >= 0 {
			return
		}
		a.key(uv.KeyEscape, 0)
		a.settled()
	}
	t.Fatalf("%s: esc did not return to the composer\nscreen:\n%s", what, a.text())
}

// TestCommandsDuringReplay runs each registered command in a replayed
// session and then checks the tape is untouched.
func TestCommandsDuringReplay(t *testing.T) {
	t.Parallel()
	tape := commandsTape(t)
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")

	// Commands that print into the transcript. Each: the output says
	// what it should, esc leaves the composer where it was, and the
	// screen holds the usual invariants.
	for _, tc := range []struct{ name, line, want string }{
		{"help", "/help", "list commands"},
		{"context", "/context", "Everything the model is told before your message"},
		{"cost", "/cost", "cost:"},
		{"keys", "/keys", "mid-turn: queue the line"},
		{"theme", "/theme", "usage: /theme <name>"},
		{"scratch", "/scratch", "scratchpad:"},
		{"todo", "/todo", "no todos"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			commandsDispatch(t, a, tc.line, tc.want)
			a.key(uv.KeyEscape, 0)
			a.settled()
			if composerRow(a.lines()) < 0 {
				t.Fatalf("%s: composer gone after esc\nscreen:\n%s", tc.line, a.text())
			}
			a.check(tc.line)
		})
	}

	// Commands that take the pane: a picker opens, esc goes back.
	for _, tc := range []struct{ name, line, want string }{
		{"model", "/model", "pick a model"},
		{"sessions", "/sessions", "resume a session"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			a.typeText(tc.line)
			a.key(uv.KeyEnter, 0)
			a.waitFor(tc.want)
			commandsCloseOverlay(t, a, tc.line)
			a.check(tc.line)
		})
	}

	// The commands above called no model: the next real input still
	// gets the first recorded reply, and the one after it the second.
	t.Run("tape_replies_intact", func(t *testing.T) {
		for _, turn := range []struct {
			in, want string
			done     int
		}{
			{"hello", "Hello there.", 1},
			{"one more", "Said hi.", 2},
		} {
			a.typeText(turn.in)
			a.key(uv.KeyEnter, 0)
			if !a.waitDone(turn.done, 60*time.Second) {
				t.Fatalf("%q: turn never finished\nscreen:\n%s", turn.in, a.text())
			}
			a.waitFor(turn.want)
			a.check("after " + turn.in)
		}
		if s := a.text(); strings.Contains(s, "end of tape") {
			t.Fatalf("a command consumed a tape reply: the tape ran out\nscreen:\n%s", s)
		}
		if n := a.doneCount(); n != 2 {
			t.Fatalf("want 2 finished turns (the tape's two non-command inputs), got %d\nscreen:\n%s", n, a.text())
		}
	})
}

// TestCommandsTapeSkipsSlashInputs replays the whole fixture: its
// "/help" input is a command, never a model call, so drive skips it
// and the two real turns still line up with the two recorded replies.
// If the "/" line were sent as a prompt, the reply for "one more"
// would be eaten and the last turn would never finish.
func TestCommandsTapeSkipsSlashInputs(t *testing.T) {
	t.Parallel()
	tape := commandsTape(t)
	entries, err := history.Read(tape)
	if err != nil {
		t.Fatal(err)
	}
	var slashInputs, commandEntries int
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		if e.Kind == "input" && strings.HasPrefix(text, "/") {
			slashInputs++
		}
		if e.Kind == "command" {
			commandEntries++
		}
	}
	if slashInputs == 0 || commandEntries == 0 {
		t.Fatalf("fixture drift: %s must keep a %q input and a command entry (got %d, %d)",
			tape, "/help", slashInputs, commandEntries)
	}
	drive(t, tape, 100, 30)
}
