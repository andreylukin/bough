package vtreal

// Several bough instances replaying one tape at once: in separate
// $HOMEs (as TestReplayHistory's parallel sweep does) and sharing one
// $HOME, so one history directory. Each instance must write its own
// session file, count only its own turns, and end on the same screen
// a solo run of the tape ends on.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/replay"
)

const multiCols, multiRows = 100, 30

// multiStart is startCfg with a caller-chosen $HOME, so two instances
// can share one.
func multiStart(t *testing.T, home, yml string) *app {
	t.Helper()
	// Written once per $HOME: rewriting it under a running instance
	// hot-reloads that instance, which is not what this test is about.
	cfg := filepath.Join(home, "bough.yml")
	if _, err := os.Stat(cfg); err != nil {
		if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	term, err := NewTerminal(t, multiCols, multiRows)
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
	a := &app{t: t, term: term, cmd: cmd, cols: multiCols, rows: multiRows, home: home}
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

// multiInstance is one running bough and the session file it owns.
type multiInstance struct {
	name    string
	a       *app
	session string
}

func multiSessions(home string) []string {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	return paths
}

// multiLaunch boots an instance in home and finds the session file it
// created: the one that was not there before it started.
func multiLaunch(t *testing.T, name, home, yml string) *multiInstance {
	t.Helper()
	before := multiSessions(home)
	a := multiStart(t, home, yml)
	a.check(name + ": boot")
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		var fresh []string
		for _, p := range multiSessions(home) {
			if !slices.Contains(before, p) {
				fresh = append(fresh, p)
			}
		}
		if len(fresh) > 1 {
			t.Fatalf("%s: boot created %d session files %v:\n%s", name, len(fresh), fresh, a.text())
		}
		if len(fresh) == 1 {
			return &multiInstance{name: name, a: a, session: fresh[0]}
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("%s: no session file of its own appeared under %s:\n%s", name, home, a.text())
	return nil
}

// multiKinds counts entries of the given kinds in one session file.
func multiKinds(path string, kinds ...string) int {
	entries, err := history.Read(path)
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range entries {
		if slices.Contains(kinds, e.Kind) {
			n++
		}
	}
	return n
}

// multiDrive types each recorded input into every instance at once,
// then waits for each to finish that turn in its own session file.
// It returns every instance's final settled screen.
func multiDrive(t *testing.T, tape string, ins ...*multiInstance) []string {
	t.Helper()
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	turns := 0
	for i, in := range tp.Inputs {
		if strings.HasPrefix(in, "/") {
			continue
		}
		turns++
		for _, m := range ins {
			m.a.typeText(in)
			m.a.key(uv.KeyEnter, 0)
		}
		for _, m := range ins {
			where := fmt.Sprintf("%s: turn %d/%d", m.name, i+1, len(tp.Inputs))
			deadline := time.Now().Add(60 * time.Second)
			for multiKinds(m.session, "done", "cancelled") < turns {
				if time.Now().After(deadline) {
					t.Fatalf("%s: turn never finished in %s:\n%s", where, m.session, m.a.text())
				}
				time.Sleep(50 * time.Millisecond)
			}
			m.a.check(where)
		}
		if t.Failed() {
			t.FailNow()
		}
	}
	// No cross-talk: each session holds exactly its own turns, and the
	// $HOME-wide doneCount is the sum of the sessions under that $HOME.
	byHome := map[string]int{}
	for _, m := range ins {
		for _, k := range []string{"done", "input"} {
			if n := multiKinds(m.session, k); n != turns {
				t.Errorf("%s: %s has %d %q entries, want %d:\n%s", m.name, m.session, n, k, turns, m.a.text())
			}
		}
		byHome[m.a.home] += turns
	}
	for _, m := range ins {
		if got, want := m.a.doneCount(), byHome[m.a.home]; got != want {
			t.Errorf("%s: doneCount under %s = %d, want %d:\n%s", m.name, m.a.home, got, want, m.a.text())
		}
	}
	screens := make([]string, len(ins))
	for i, m := range ins {
		screens[i] = m.a.settled()
	}
	return screens
}

// multiPromptSize is the context chip's system-prompt size, which
// carries the $HOME path and so differs by temp-dir name length.
var multiPromptSize = regexp.MustCompile(`\d+ chars in the system prompt`)

func multiSame(t *testing.T, solo string, screens []string, ins []*multiInstance) {
	t.Helper()
	norm := func(s string) string { return multiPromptSize.ReplaceAllString(s, "N chars in the system prompt") }
	for i, s := range screens {
		if norm(s) != norm(solo) {
			t.Errorf("%s: final screen differs from the solo run\n--- solo:\n%s\n--- %s:\n%s", ins[i].name, solo, ins[i].name, s)
		}
	}
}

func TestMultiInstance(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
	yml := replayConfig(tape)

	solo := multiLaunch(t, "solo", t.TempDir(), yml)
	want := multiDrive(t, tape, solo)[0]

	t.Run("TestMultiInstanceSeparateHomes", func(t *testing.T) {
		t.Parallel()
		ins := []*multiInstance{
			multiLaunch(t, "a", t.TempDir(), yml),
			multiLaunch(t, "b", t.TempDir(), yml),
		}
		screens := multiDrive(t, tape, ins...)
		for _, m := range ins {
			if got := multiSessions(m.a.home); len(got) != 1 {
				t.Errorf("%s: %d session files under its own $HOME, want 1: %v\n%s", m.name, len(got), got, m.a.text())
			}
		}
		multiSame(t, want, screens, ins)
	})

	t.Run("TestMultiInstanceSharedHome", func(t *testing.T) {
		t.Parallel()
		home := t.TempDir()
		ins := []*multiInstance{multiLaunch(t, "a", home, yml)}
		ins = append(ins, multiLaunch(t, "b", home, yml))
		if ins[0].session == ins[1].session {
			t.Fatalf("both instances write %s:\n%s", ins[0].session, ins[1].a.text())
		}
		screens := multiDrive(t, tape, ins...)
		if got := multiSessions(home); len(got) != 2 {
			t.Errorf("%d session files in the shared history dir, want 2: %v\n%s", len(got), got, ins[0].a.text())
		}
		multiSame(t, want, screens, ins)
	})
}
