package vtreal

// `bough --headless` run against the same $HOME while a TUI session
// sits idle there: both share the history directory and the graph's
// sqlite database. The headless session must show up in the TUI's
// /sessions picker, neither process may hit "database is locked", and
// the TUI's own next turn must still persist, to its session file and
// to graph.db.

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/graph"
)

// headlessConcurrentWithTUISameDBGraph adds the sqlite-backed graph row
// (no embedder: no network) to a replay config.
const headlessConcurrentWithTUISameDBGraph = `
- id: graph
  plugin: graph
  config: {embed: false}
`

const headlessConcurrentWithTUISameDBLocked = "database is locked"

// headlessConcurrentWithTUISameDBRun runs `bough --headless` in home
// with its own config file, so the TUI's bough.yml is never rewritten
// (that would hot-reload the TUI).
func headlessConcurrentWithTUISameDBRun(t *testing.T, home, tape, prompt string) headlessResult {
	t.Helper()
	cfg := filepath.Join(home, "headless.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)+headlessConcurrentWithTUISameDBGraph), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = strings.NewReader(prompt + "\n")
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	runErr := cmd.Run()
	res := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		res.code = ee.ExitCode()
	} else if runErr != nil {
		t.Fatalf("running bough --headless: %v\n%s", runErr, res.screen())
	}
	if ctx.Err() != nil {
		t.Fatalf("bough --headless never exited:\n%s", res.screen())
	}
	return res
}

// headlessConcurrentWithTUISameDBWaitDone polls a session file until it
// holds n done entries.
func headlessConcurrentWithTUISameDBWaitDone(t *testing.T, m *multiInstance, n int) {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for multiKinds(m.session, "done") < n {
		if time.Now().After(deadline) {
			t.Fatalf("%s: turn %d never landed in %s:\n%s", m.name, n, m.session, m.a.text())
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func TestHeadlessConcurrentWithTUISameDB(t *testing.T) {
	t.Parallel()
	tuiTape, _ := filepath.Abs("testdata/replay/follow-up.jsonl")
	hlTape, _ := filepath.Abs("testdata/replay/headless_answer.jsonl")
	home := t.TempDir()

	// 1. The TUI finishes one turn, then sits idle.
	tui := multiLaunch(t, "tui", home, replayConfig(tuiTape)+headlessConcurrentWithTUISameDBGraph)
	tui.a.typeText("alpha")
	tui.a.key(uv.KeyEnter, 0)
	headlessConcurrentWithTUISameDBWaitDone(t, tui, 1)
	tui.a.waitFor("REPLY-ALPHA")
	tui.a.check("tui: idle after turn 1")

	// 2. Headless runs a whole turn in the same $HOME.
	before := multiSessions(home)
	r := headlessConcurrentWithTUISameDBRun(t, home, hlTape, "how many go files are here")
	var hlSession string
	for _, p := range multiSessions(home) {
		if !slices.Contains(before, p) {
			hlSession = p
		}
	}

	t.Run("HeadlessCleanNoLock", func(t *testing.T) {
		if r.code != 0 {
			t.Errorf("headless beside an idle TUI must exit 0:\n%s", r.screen())
		}
		if strings.Contains(r.stdout+r.stderr, headlessConcurrentWithTUISameDBLocked) {
			t.Errorf("headless hit a locked database:\n%s", r.screen())
		}
		if !strings.Contains(r.stdout, "Two Go files: a.go and b.go.") {
			t.Errorf("headless answer missing:\n%s", r.screen())
		}
		if hlSession == "" {
			t.Fatalf("headless wrote no session file of its own under %s:\n%s", home, r.screen())
		}
		if n := multiKinds(hlSession, "done"); n != 1 {
			t.Errorf("headless session %s has %d done entries, want 1", hlSession, n)
		}
	})

	t.Run("PickerShowsHeadlessSession", func(t *testing.T) {
		newSessionOpenPicker(tui.a)
		tui.a.waitFor("how many go files are here")
		tui.a.key(uv.KeyEscape, 0)
		tui.a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
	})

	t.Run("TUINextTurnPersists", func(t *testing.T) {
		tui.a.typeText("beta")
		tui.a.key(uv.KeyEnter, 0)
		headlessConcurrentWithTUISameDBWaitDone(t, tui, 2)
		tui.a.waitFor("REPLY-BETA")
		tui.a.check("tui: after turn 2")
		if s := tui.a.text(); strings.Contains(s, headlessConcurrentWithTUISameDBLocked) {
			t.Errorf("tui shows a locked database:\n%s", s)
		}
		if n := multiKinds(tui.session, "input"); n != 2 {
			t.Errorf("tui session has %d inputs, want 2", n)
		}
		if hlSession != "" && multiKinds(hlSession, "input") != 1 {
			t.Errorf("tui turn leaked into the headless session file %s", hlSession)
		}
	})

	t.Run("GraphHoldsBothSessions", func(t *testing.T) {
		st, err := graph.Open(filepath.Join(home, ".bough", "graph.db"))
		if err != nil {
			t.Fatalf("open graph.db beside a live TUI: %v", err)
		}
		defer st.Close()
		for name, p := range map[string]string{"tui": tui.session, "headless": hlSession} {
			if p == "" {
				continue
			}
			id := strings.TrimSuffix(filepath.Base(p), ".jsonl")
			if _, err := st.Get("session", id); err != nil {
				t.Errorf("graph.db has no session entity for the %s session %s: %v", name, id, err)
			}
		}
	})
}
