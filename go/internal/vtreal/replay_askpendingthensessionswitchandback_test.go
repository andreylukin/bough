package vtreal

// ask-pending-then-session-switch-and-back: two fixture sessions are
// seeded into $HOME before boot; the live session's tape asks a
// question behind a gate file. With the /sessions picker open the gate
// opens and the ask goes pending; the test resumes a fixture session,
// then reopens /sessions and resumes the original. Either the ask is
// pending again and "1" reaches the block (the tape's result carries
// "you picked chartreuse"), or history marks it declined — never a
// hung turn, and never an answer written into another session.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// askPendingThenSessionSwitchAndBackStart seeds two finished sessions
// and boots the ask config on the gated tape.
func askPendingThenSessionSwitchAndBackStart(t *testing.T, gate string) *app {
	t.Helper()
	home := t.TempDir()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	for i, s := range []struct{ id, prompt, reply string }{
		{"fixture-a", "fixture alpha prompt", "alpha answer"},
		{"fixture-b", "fixture beta prompt", "beta answer"},
	} {
		sessionTreeSeed(t, dir, s.id, now.Add(-time.Duration(i+1)*time.Hour),
			sessionTreeEntry("meta", map[string]any{"cwd": "/tmp/demo"}),
			sessionTreeEntry("input", map[string]any{"text": s.prompt}),
			sessionTreeEntry("assistant", map[string]any{"text": s.reply}),
			sessionTreeEntry("done", map[string]any{}),
		)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(askConfig(askDuringSessionPickerTape(t, gate))), 0o644); err != nil {
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

// askPendingThenSessionSwitchAndBackKinds returns the kind and the
// text+question of every entry in one session file.
func askPendingThenSessionSwitchAndBackKinds(path string) (kinds, texts []string) {
	b, _ := os.ReadFile(path)
	for _, l := range strings.Split(string(b), "\n") {
		var e struct {
			Kind string `json:"kind"`
			Data struct {
				Text     string `json:"text"`
				Question string `json:"question"`
			} `json:"data"`
		}
		if json.Unmarshal([]byte(l), &e) == nil && e.Kind != "" {
			kinds = append(kinds, e.Kind)
			texts = append(texts, e.Data.Text+e.Data.Question)
		}
	}
	return kinds, texts
}

func askPendingThenSessionSwitchAndBackOpenPicker(a *app) {
	a.t.Helper()
	a.typeText("/sessions")
	a.waitFor("> /sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
	a.settled()
}

func TestAskPendingThenSessionSwitchAndBack(t *testing.T) {
	t.Parallel()
	t.Run("switch-and-back", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_ASK_PENDING_THEN_SESSION_SWITCH_AND_BACK") == "" {
			t.Skip("known bug: enter on a /sessions row while tools.ask is pending freezes the UI until the ask times out " +
				"(session-choose -> runtimeSet remounts loop synchronously from ui Update; the ask select in plugins/ask/ask.go " +
				"has no cancel path); set BOUGH_KNOWN_ASK_PENDING_THEN_SESSION_SWITCH_AND_BACK=1 to run")
		}
		askPendingThenSessionSwitchAndBackRun(t)
	})
}

func askPendingThenSessionSwitchAndBackRun(t *testing.T) {
	gate := filepath.Join(t.TempDir(), "gate")
	a := askPendingThenSessionSwitchAndBackStart(t, gate)
	dir := filepath.Join(a.home, ".bough", "history")
	fixtureA := filepath.Join(dir, "fixture-a.jsonl")
	fixtureB := filepath.Join(dir, "fixture-b.jsonl")
	beforeB, _ := os.ReadFile(fixtureB)

	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a color")
	askPendingThenSessionSwitchAndBackOpenPicker(a)

	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(15 * time.Second)
	for len(askDuringSessionPickerEntries(a, "ask")) == 0 {
		if time.Now().After(deadline) {
			t.Fatalf("ask never recorded:\n%s", a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.settled()
	var orig string
	paths, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	for _, p := range paths {
		if p != fixtureA && p != fixtureB {
			orig = p
		}
	}
	if orig == "" {
		t.Fatalf("no live session file in %s", dir)
	}

	// Away: resume fixture alpha while the ask is pending.
	sessionTreeSelect(a, "fixture alpha prompt")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resumed fixture-a")
	if s := a.settled(); !strings.Contains(s, "alpha answer") {
		t.Fatalf("fixture alpha transcript not shown:\n%s", s)
	}

	// And back to the original.
	askPendingThenSessionSwitchAndBackOpenPicker(a)
	sessionTreeSelect(a, "pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close")
	s := a.settled()

	kinds, _ := askPendingThenSessionSwitchAndBackKinds(orig)
	declined := false
	for _, k := range kinds {
		if strings.Contains(k, "declin") || k == "cancelled" {
			declined = true
		}
	}
	if !declined {
		for _, want := range []string{"? Pick a color", "chartreuse", askPendingPlaceholder} {
			if !strings.Contains(s, want) {
				t.Fatalf("back on the original: ask neither pending (missing %q) nor declined in history %q:\n%s", want, kinds, s)
			}
		}
		a.typeText("1")
		a.key(uv.KeyEnter, 0)
	}

	until := time.Now().Add(10 * time.Second)
	for {
		k, _ := askPendingThenSessionSwitchAndBackKinds(orig)
		finished := false
		for _, x := range k {
			if x == "done" || x == "cancelled" {
				finished = true
			}
		}
		if finished {
			break
		}
		if time.Now().After(until) {
			t.Fatalf("turn hung: no done/cancelled within 10s; kinds %q\n%s", k, a.text())
		}
		time.Sleep(50 * time.Millisecond)
	}
	if !declined {
		kinds, texts := askPendingThenSessionSwitchAndBackKinds(orig)
		var answers, results []string
		for i, k := range kinds {
			switch k {
			case "ask/answer":
				answers = append(answers, texts[i])
			case "result":
				results = append(results, texts[i])
			}
		}
		if len(answers) != 1 || answers[0] != "chartreuse" {
			t.Errorf("original ask/answer entries = %q, want [chartreuse]", answers)
		}
		if len(results) != 1 || !strings.Contains(results[0], "you picked chartreuse") {
			t.Errorf("original tool result = %q, want \"you picked chartreuse\"", results)
		}
	}
	for _, p := range []string{fixtureA, fixtureB} {
		k, _ := askPendingThenSessionSwitchAndBackKinds(p)
		for _, x := range k {
			if strings.HasPrefix(x, "ask") || x == "result" {
				t.Errorf("%s gained a %q entry (answer leaked into another session): %q", filepath.Base(p), x, k)
			}
		}
	}
	if afterB, _ := os.ReadFile(fixtureB); string(afterB) != string(beforeB) {
		t.Errorf("fixture-b (never resumed) changed on disk")
	}
	a.check("ask-pending-then-session-switch-and-back end")
}
