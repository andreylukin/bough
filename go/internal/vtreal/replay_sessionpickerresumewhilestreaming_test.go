package vtreal

// session-picker-resume-while-streaming: session B's turn is streaming
// (the cancel tape: ALPHASTART, filler, ALPHAEND) when /sessions is
// opened and an older, pre-seeded session A is picked. Either the pick
// is refused with a visible notice, or B's turn is cancelled first and
// A renders clean: none of B's text on screen afterwards, B's history
// file ends with a cancelled entry, and A's file gains nothing of B.
// "stalled" pauses the tape after the first delta; "streaming" keeps
// deltas arriving while the picker is open.

import (
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// sessionPickerResumeWhileStreamingBText is what only session B wrote.
var sessionPickerResumeWhileStreamingBText = []string{"ALPHASTART", "filler", "ALPHAEND", "start the long one"}

// sessionPickerResumeWhileStreamingKinds lists the entry kinds of one
// session file.
func sessionPickerResumeWhileStreamingKinds(t *testing.T, path string) []string {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	return kinds
}

func TestSessionPickerResumeWhileStreaming(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		delayMS int
	}{
		{"stalled", streamStallCancelDelayMS},
		{"streaming", 120},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			sessionPickerResumeWhileStreamingRun(t, tc.delayMS)
		})
	}
}

func sessionPickerResumeWhileStreamingRun(t *testing.T, delayMS int) {
	home := t.TempDir()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	sessionTreeSeed(t, dir, "older-a", time.Now().Add(-2*time.Hour),
		sessionTreeEntry("meta", map[string]any{"cwd": home}),
		sessionTreeEntry("input", map[string]any{"text": "older session prompt"}),
		sessionTreeEntry("assistant", map[string]any{"text": "```stop\nOLDERANSWER\n```"}),
		sessionTreeEntry("done", map[string]any{}),
	)
	aPath := filepath.Join(dir, "older-a.jsonl")
	aBefore, _ := os.ReadFile(aPath)

	a := sessionPickerResumeWhileStreamingStart(t, home, cancelConfig(cancelTape(t), delayMS))
	a.typeText("start the long one")
	a.key(uv.KeyEnter, 0)
	a.waitFor("ALPHASTART")

	a.typeText("/sessions")
	a.waitFor("> /sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
	sessionTreeSelect(a, "older session prompt")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close")

	s := a.settled()
	if !strings.Contains(s, "OLDERANSWER") {
		// Refusal is allowed only with a visible reason.
		if l := strings.ToLower(s); !strings.Contains(l, "running") && !strings.Contains(l, "cancel") {
			t.Fatalf("pick of A neither resumed it nor showed a notice:\n%s", s)
		}
		a.key(uv.KeyEsc, 0)
	} else {
		// Resumed: wait out any late delta, then none of B may show.
		time.Sleep(1500 * time.Millisecond)
		s = a.settled()
		for _, bad := range sessionPickerResumeWhileStreamingBText {
			if strings.Contains(s, bad) {
				t.Errorf("A's transcript shows B's %q after the resume:\n%s", bad, s)
			}
		}
		if cancelSpinner.MatchString(s) {
			t.Errorf("B's turn still running (spinner) after resuming A:\n%s", s)
		}
	}
	a.check("after the pick")

	// History: B's file ends cancelled; A's file gained nothing of B.
	paths, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	var bPath string
	for _, p := range paths {
		if p != aPath {
			bPath = p
		}
	}
	if bPath == "" {
		t.Fatalf("no history file for session B in %v", paths)
	}
	deadline := time.Now().Add(5 * time.Second)
	var kinds []string
	for {
		kinds = sessionPickerResumeWhileStreamingKinds(t, bPath)
		if slices.Contains(kinds, "cancelled") || time.Now().After(deadline) {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if !slices.Contains(kinds, "cancelled") { // the loop writes cancelled, then done
		t.Errorf("B's history has no cancelled entry: kinds %v", kinds)
	}
	aAfter, _ := os.ReadFile(aPath)
	for _, bad := range sessionPickerResumeWhileStreamingBText {
		if strings.Contains(string(aAfter), bad) && !strings.Contains(string(aBefore), bad) {
			t.Errorf("B's %q was appended to A's history:\n%s", bad, aAfter)
		}
	}
}

// sessionPickerResumeWhileStreamingStart boots yml over an existing home.
func sessionPickerResumeWhileStreamingStart(t *testing.T, home, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}
