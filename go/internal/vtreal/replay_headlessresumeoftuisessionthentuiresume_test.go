package vtreal

// One session across three processes: a TUI records two turns (and a
// todo item) and quits, `bough --headless --resume <id>` adds a third
// turn to the same file, and a second TUI resumes it. The resumed TUI
// must show all three turns in order, the summed usage, the title the
// session already had, and the todo list from before the headless run.
//
// The replay plugin reports no usage and session-title is off under
// replay, so the usage and the title are stamped into the file the way
// a priced provider and the session-title row would have written them.

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const (
	headlessResumeOfTUISessionThenTUIResumeID    = "hrtui0001"
	headlessResumeOfTUISessionThenTUIResumeTitle = "Release prep chat"
	headlessResumeOfTUISessionThenTUIResumeTodo  = "ship after two"
)

// headlessResumeOfTUISessionThenTUIResumeStart boots the TUI in home
// with its own config file name, so a second boot in the same $HOME
// gets its own config and nothing is hot-reloaded.
func headlessResumeOfTUISessionThenTUIResumeStart(t *testing.T, home, name, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, name)
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	const cols, rows = 100, 50
	term, err := NewTerminal(t, cols, rows)
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
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
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

// headlessResumeOfTUISessionThenTUIResumeQuit quits the TUI and waits
// for the process to exit, so its last writes are on disk.
func headlessResumeOfTUISessionThenTUIResumeQuit(t *testing.T, a *app) {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- a.cmd.Wait() }()
	a.key('c', uv.ModCtrl)
	a.key('c', uv.ModCtrl)
	select {
	case <-done:
	case <-time.After(15 * time.Second):
		t.Fatalf("TUI did not exit on ctrl+c ctrl+c:\n%s", a.text())
	}
}

// headlessResumeOfTUISessionThenTUIResumeAppend adds one entry to the
// end of the session file.
func headlessResumeOfTUISessionThenTUIResumeAppend(t *testing.T, path, kind string, data map[string]any) {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	e := history.Entry{Seq: int64(len(entries) + 1), At: time.Now(), Kind: kind, Data: data}
	line, err := json.Marshal(e)
	if err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if _, err := f.Write(append(line, '\n')); err != nil {
		t.Fatal(err)
	}
}

// headlessResumeOfTUISessionThenTUIResumeRows is the session file's
// entries of the given kinds, as "kind:text" rows in file order.
func headlessResumeOfTUISessionThenTUIResumeRows(t *testing.T, path string, kinds ...string) []string {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, e := range entries {
		for _, k := range kinds {
			if e.Kind == k {
				txt, _ := e.Data["text"].(string)
				out = append(out, e.Kind+":"+txt)
			}
		}
	}
	return out
}

func TestHeadlessResumeOfTUISessionThenTUIResume(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	home := t.TempDir()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	log := filepath.Join(dir, headlessResumeOfTUISessionThenTUIResumeID+".jsonl")

	// 1. TUI: two turns and a todo item, then quit.
	first := headlessResumeOfTUISessionThenTUIResumeStart(t, home, "tui1.yml", resumeConfig(tape, log))
	resumeSend(t, first, log, "greet me", 1)
	resumeSend(t, first, log, "count the files", 2)
	first.typeText("/todo add " + headlessResumeOfTUISessionThenTUIResumeTodo)
	first.key(uv.KeyEnter, 0)
	first.waitFor(todoHeader)
	first.check("first TUI")
	headlessResumeOfTUISessionThenTUIResumeQuit(t, first)
	headlessResumeOfTUISessionThenTUIResumeAppend(t, log, "title",
		map[string]any{"text": headlessResumeOfTUISessionThenTUIResumeTitle})
	titlesBefore := headlessResumeOfTUISessionThenTUIResumeRows(t, log, "title")
	todosBefore := headlessResumeOfTUISessionThenTUIResumeRows(t, log, "todo/add", "todo/done")
	if len(todosBefore) == 0 {
		t.Fatalf("/todo add wrote no todo entry to %s", log)
	}

	// 2. Headless resumes the session by id for a third turn.
	hcfg := filepath.Join(home, "headless.yml")
	if err := os.WriteFile(hcfg, []byte(replayConfig(next)), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "-config", hcfg, "--headless", "--resume", headlessResumeOfTUISessionThenTUIResumeID)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = strings.NewReader("and now\n")
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	runErr := cmd.Run()
	r := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		r.code = ee.ExitCode()
	} else if runErr != nil {
		t.Fatalf("running bough --headless --resume: %v\n%s", runErr, r.screen())
	}
	if r.code != 0 || !strings.Contains(r.stdout, "Continued on a fresh tape.") {
		t.Fatalf("headless resume turn did not answer cleanly:\n%s", r.screen())
	}
	if got := headlessResumeOfTUISessionThenTUIResumeRows(t, log, "input"); strings.Join(got, "|") != "input:greet me|input:count the files|input:and now" {
		t.Fatalf("session file inputs = %q, want the headless turn appended to the TUI's session", got)
	}
	if all, _ := filepath.Glob(filepath.Join(dir, "*.jsonl")); len(all) != 1 {
		t.Errorf("headless --resume created another session file: %v", all)
	}
	resumeStampUsage(t, log, []map[string]any{
		{"in": 10000, "out": 2000, "cost": 0.03, "last_in": 4000},
		{"in": 2000, "out": 1000, "cost": 0.02, "last_in": 4000},
		{"in": 345, "out": 456, "cost": 0.002, "last_in": 4000},
	})

	// 3. The TUI resumes the session the headless run grew.
	second := headlessResumeOfTUISessionThenTUIResumeStart(t, home, "tui2.yml", resumeConfig(next, log))
	second.waitFor("resumed ")
	second.check("resumed TUI")
	s := second.settled()

	t.Run("ThreeTurnsInOrder", func(t *testing.T) {
		at := -1
		for _, want := range []string{"greet me", "Hello from turn one.", "count the files", "Two files here.", "and now", "Continued on a fresh tape."} {
			i := strings.Index(s[at+1:], want)
			if i < 0 {
				t.Fatalf("resumed transcript is missing %q after position %d:\n%s", want, at, s)
			}
			at += 1 + i
		}
	})

	t.Run("StatusBarUsageIsSum", func(t *testing.T) {
		if !strings.Contains(s, "↑12.3k ↓3.5k · $0.052") {
			t.Fatalf("status bar does not show the three turns' sum (↑12.3k ↓3.5k · $0.052):\n%s", s)
		}
	})

	t.Run("TitleUnchanged", func(t *testing.T) {
		if !strings.Contains(s, headlessResumeOfTUISessionThenTUIResumeTitle) {
			t.Errorf("status bar lost the session title %q:\n%s", headlessResumeOfTUISessionThenTUIResumeTitle, s)
		}
		if got := headlessResumeOfTUISessionThenTUIResumeRows(t, log, "title"); strings.Join(got, "|") != strings.Join(titlesBefore, "|") {
			t.Errorf("title entries changed across the headless run: %q, want %q", got, titlesBefore)
		}
	})

	t.Run("TodoFromTurnTwoPreserved", func(t *testing.T) {
		second.waitFor(todoHeader)
		if s := second.settled(); !strings.Contains(s, "[ ] 1 "+headlessResumeOfTUISessionThenTUIResumeTodo) {
			t.Errorf("resumed todo panel lost %q:\n%s", headlessResumeOfTUISessionThenTUIResumeTodo, s)
		}
		if got := headlessResumeOfTUISessionThenTUIResumeRows(t, log, "todo/add", "todo/done"); strings.Join(got, "|") != strings.Join(todosBefore, "|") {
			t.Errorf("todo entries changed across the headless run: %q, want %q", got, todosBefore)
		}
	})
}
