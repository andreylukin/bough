package vtreal

// The /sessions picker and /new on a real PTY: rows come from the
// run's own $HOME/.bough/history, arrow+enter resumes, esc goes back,
// and /new opens a fresh file whose meta records the working directory.
//
// The "/new <dir>" dialog with fzf-style directory autocomplete is
// covered in replay_resizeduringnewsessiondialogfzf_test.go.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// newSessionSeed writes a finished session with one prompt/reply under
// the run's history dir.
func newSessionSeed(t *testing.T, a *app, id, cwd, prompt string) {
	t.Helper()
	dir := filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Now().UTC().Format(time.RFC3339)
	var sb strings.Builder
	for i, e := range []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": cwd}},
		{"input", map[string]any{"text": prompt}},
		{"assistant", map[string]any{"text": "reply to " + prompt}},
		{"done", map[string]any{}},
	} {
		b, _ := json.Marshal(map[string]any{"seq": i + 1, "at": at, "kind": e.kind, "data": e.data})
		sb.Write(append(b, '\n'))
	}
	if err := os.WriteFile(filepath.Join(dir, id+".jsonl"), []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}

func newSessionOpenPicker(a *app) {
	a.t.Helper()
	a.typeText("/sessions")
	a.waitFor("> /sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
}

func TestNewSessionPicker(t *testing.T) {
	t.Parallel()

	t.Run("ListsHistory", func(t *testing.T) {
		t.Parallel()
		a := start(t, 120, 30)
		newSessionSeed(t, a, "seed-alpha", "/elsewhere/alpha", "alpha prompt")
		newSessionSeed(t, a, "seed-beta", "/elsewhere/beta", "beta prompt")
		newSessionOpenPicker(a)
		s := a.settled()
		for _, want := range []string{"alpha prompt", "beta prompt", "/elsewhere/alpha", "esc back"} {
			if !strings.Contains(s, want) {
				t.Errorf("picker missing %q:\n%s", want, s)
			}
		}
		if !strings.Contains(s, "▸ ") {
			t.Errorf("no selected row:\n%s", s)
		}
	})

	t.Run("ArrowEnterResumes", func(t *testing.T) {
		t.Parallel()
		a := start(t, 120, 30)
		newSessionSeed(t, a, "seed-alpha", "/elsewhere/alpha", "alpha prompt")
		newSessionSeed(t, a, "seed-beta", "/elsewhere/beta", "beta prompt")
		newSessionOpenPicker(a)
		// Walk down until the selected row is beta, wherever it sorts.
		selected := func() bool {
			for _, l := range a.lines() {
				if strings.Contains(l, "▸ ") && strings.Contains(l, "beta prompt") {
					return true
				}
			}
			return false
		}
		for i := 0; i < 5 && !selected(); i++ {
			a.key(uv.KeyDown, 0)
			a.settled()
		}
		if !selected() {
			t.Fatalf("down never selected beta:\n%s", a.text())
		}
		a.key(uv.KeyEnter, 0)
		a.waitFor("resumed seed-beta")
		s := a.settled()
		if !strings.Contains(s, "reply to beta prompt") || strings.Contains(s, "alpha prompt") {
			t.Errorf("resumed transcript is not beta's:\n%s", s)
		}
		a.check("after resume")
	})

	t.Run("EscCloses", func(t *testing.T) {
		t.Parallel()
		a := start(t, 100, 30)
		newSessionSeed(t, a, "seed-alpha", "/elsewhere/alpha", "alpha prompt")
		newSessionOpenPicker(a)
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
		s := a.settled()
		if strings.Contains(s, "resumed seed-alpha") {
			t.Errorf("esc resumed a session:\n%s", s)
		}
		a.check("after esc")
	})
}

func TestNewSessionSlashNewRecordsCwd(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("hello")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: hello")
	dir := filepath.Join(a.home, ".bough", "history")
	before, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	a.typeText("/new")
	a.waitFor("> /new")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "● bough") }, "/new to clear the transcript")
	var fresh string
	for deadline := time.Now().Add(20 * time.Second); fresh == "" && time.Now().Before(deadline); {
		paths, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
		for _, p := range paths {
			if !slices.Contains(before, p) {
				fresh = p
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	if fresh == "" {
		t.Fatalf("/new wrote no new history file (had %v):\n%s", before, a.text())
	}
	var cwd string
	for range 100 { // the meta line may land just after the file
		if es, err := history.Read(fresh); err == nil && len(es) > 0 && es[0].Kind == "meta" {
			cwd, _ = es[0].Data["cwd"].(string)
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	want, _ := filepath.EvalSymlinks(a.home)
	got, _ := filepath.EvalSymlinks(cwd)
	if cwd == "" || got != want {
		t.Errorf("new session meta cwd = %q, want %q:\n%s", cwd, a.home, a.text())
	}
	a.check("after /new")
}
