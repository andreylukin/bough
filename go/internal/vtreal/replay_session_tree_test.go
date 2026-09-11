package vtreal

// The session tree: /sessions opens the picker over the real history
// directory, which nests a fork under the session it was forked from
// (meta.forked_from). A root session carrying a subagent (sub:* entries
// inline — bough keeps no separate file per subagent) and a fork of it
// are seeded into a fresh $HOME before boot; the tests check the tree
// rows, that enter resumes the selected session with its subagent card
// collapsed, and that esc closes the picker.

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

// sessionTreeSeed writes one session file into dir at mtime.
func sessionTreeSeed(t *testing.T, dir, id string, mtime time.Time, entries ...map[string]any) {
	t.Helper()
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = mtime.Format(time.RFC3339)
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(dir, id+".jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(p, mtime, mtime); err != nil {
		t.Fatal(err)
	}
}

func sessionTreeEntry(kind string, data map[string]any) map[string]any {
	return map[string]any{"kind": kind, "data": data}
}

// sessionTreeStart boots the echo config over a $HOME whose history
// holds "root" (with a finished subagent) and "fork" (forked from
// root), then opens the picker with /sessions.
func sessionTreeStart(t *testing.T) *app {
	t.Helper()
	home := t.TempDir()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	sessionTreeSeed(t, dir, "root", now.Add(-2*time.Hour),
		sessionTreeEntry("meta", map[string]any{"cwd": "/tmp/tree"}),
		sessionTreeEntry("input", map[string]any{"text": "root prompt"}),
		sessionTreeEntry("assistant", map[string]any{"text": "spawning a helper"}),
		sessionTreeEntry("sub:start", map[string]any{"text": "helper task", "worker": 1}),
		sessionTreeEntry("sub:assistant", map[string]any{"text": "SUBREPORT-hidden-when-collapsed", "worker": 1}),
		sessionTreeEntry("sub:done", map[string]any{"status": "ok", "steps": 1, "text": "", "worker": 1}),
		sessionTreeEntry("done", map[string]any{}),
	)
	sessionTreeSeed(t, dir, "fork", now.Add(-time.Hour),
		sessionTreeEntry("meta", map[string]any{"cwd": "/tmp/tree",
			"forked_from": filepath.Join(dir, "root.jsonl"), "at_seq": 2}),
		sessionTreeEntry("input", map[string]any{"text": "fork prompt"}),
		sessionTreeEntry("assistant", map[string]any{"text": "fork answer"}),
		sessionTreeEntry("done", map[string]any{}),
	)

	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
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
	a.typeText("/sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")
	return a
}

// sessionTreeRow is the index of the first screen row containing s.
func sessionTreeRow(a *app, s string) int {
	for i, l := range a.lines() {
		if strings.Contains(l, s) {
			return i
		}
	}
	return -1
}

// sessionTreeSelect presses down until the ▸ marker is on the row
// containing s.
func sessionTreeSelect(a *app, s string) {
	a.t.Helper()
	for range 10 {
		if r := sessionTreeRow(a, s); r >= 0 && strings.HasPrefix(a.lines()[r], "▸ ") {
			return
		}
		a.key(uv.KeyDown, 0)
		a.settled()
	}
	a.t.Fatalf("could not select the %q row\nscreen:\n%s", s, a.text())
}

func TestSessionTreeRows(t *testing.T) {
	t.Parallel()
	a := sessionTreeStart(t)
	s := a.settled()
	ls := a.lines()
	root, fork := sessionTreeRow(a, "root prompt"), sessionTreeRow(a, "fork prompt")
	if root < 0 || fork < 0 {
		t.Fatalf("root or fork row missing:\n%s", s)
	}
	if fork != root+1 {
		t.Errorf("fork (row %d) not directly under its root (row %d):\n%s", fork, root, s)
	}
	if !strings.Contains(ls[fork], "└─ ") {
		t.Errorf("fork row has no └─ connector: %q\n%s", ls[fork], s)
	}
	if strings.ContainsAny(ls[root], "├└│") {
		t.Errorf("root row carries a tree connector: %q\n%s", ls[root], s)
	}
	if !strings.Contains(s, "(current)") || !strings.Contains(s, "esc back") {
		t.Errorf("current session mark or mid-session hint missing:\n%s", s)
	}
	if strings.Contains(s, "helper task") {
		t.Errorf("a subagent shows up as a session row:\n%s", s)
	}
}

func TestSessionTreeEscCloses(t *testing.T) {
	t.Parallel()
	a := sessionTreeStart(t)
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "picker to close")
	s := a.settled()
	if composerRow(a.lines()) < 0 || strings.Contains(s, "resumed ") {
		t.Errorf("esc did not go back to the unchanged chat:\n%s", s)
	}
}

func TestSessionTreeEnterResumesRootWithSubagentCollapsed(t *testing.T) {
	t.Parallel()
	a := sessionTreeStart(t)
	sessionTreeSelect(a, "root prompt")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resumed root")
	s := a.settled()
	if strings.Contains(s, "resume a session") || !strings.Contains(s, "root prompt") {
		t.Errorf("root transcript not shown after enter:\n%s", s)
	}
	card := sessionTreeRow(a, "subagent 1")
	if card < 0 {
		t.Fatalf("subagent card missing from the resumed transcript:\n%s", s)
	}
	if l := a.lines()[card]; !strings.Contains(l, "▸") {
		t.Errorf("subagent card is not collapsed: %q\n%s", l, s)
	}
	if strings.Contains(s, "SUBREPORT-hidden-when-collapsed") {
		t.Errorf("collapsed subagent card shows its report:\n%s", s)
	}
}

func TestSessionTreeEnterResumesFork(t *testing.T) {
	t.Parallel()
	a := sessionTreeStart(t)
	sessionTreeSelect(a, "fork prompt")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resumed fork")
	if s := a.settled(); !strings.Contains(s, "fork answer") || strings.Contains(s, "root prompt") {
		t.Errorf("fork transcript wrong after enter:\n%s", s)
	}
}
