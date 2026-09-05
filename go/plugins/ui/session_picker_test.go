package ui

// Mid-session picker tests: /sessions opens the picker over a real
// history directory (this directory's sessions first, current marked,
// cwd column), esc goes back, enter swaps through the choose seam and
// replays; /sessions <id> resumes directly; the status bar click opens
// the picker; a resumed transcript ends with the "resumed" row.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

// sessionsReg mirrors the built-in /sessions: no args opens the
// picker, an id resumes it.
func sessionsReg(t *testing.T) *commands.Registry {
	t.Helper()
	r := commands.NewRegistry()
	err := r.Register(commands.CommandInfo{Name: "sessions", Usage: "[id]", Summary: "pick"},
		func(args string) (string, error) {
			if id := strings.TrimSpace(args); id != "" {
				return "", commands.ResumeAction(id)
			}
			return "", commands.ActionOpenPicker
		})
	if err != nil {
		t.Fatal(err)
	}
	return r
}

// writeJSONL stores one session in dir with a meta cwd entry and one
// input, at the given mtime.
func writeJSONL(t *testing.T, dir, id, cwd, prompt string, mtime time.Time) string {
	t.Helper()
	p := filepath.Join(dir, id+".jsonl")
	body := jsonLine(t, map[string]any{"seq": 1, "kind": "meta", "data": map[string]any{"cwd": cwd}}) + "\n" +
		jsonLine(t, map[string]any{"seq": 2, "kind": "input", "data": map[string]any{"text": prompt}}) + "\n" +
		jsonLine(t, map[string]any{"seq": 3, "kind": "assistant", "data": map[string]any{"text": "answer for " + id}}) + "\n"
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(p, mtime, mtime); err != nil {
		t.Fatal(err)
	}
	return p
}

// midSession builds a driver on a real session directory: the current
// session "cur" (this cwd), a newer "other" session from another
// project, and an older "here" session from this cwd. choose records
// the id and swaps the cfg to the chosen file's transcript.
func midSession(t *testing.T) (*drv, chan string) {
	t.Helper()
	dir := t.TempDir()
	cwd, _ := os.Getwd()
	now := time.Now()
	cur := writeJSONL(t, dir, "cur", cwd, "current prompt", now.Add(-time.Minute))
	writeJSONL(t, dir, "other", "/elsewhere/project", "other prompt", now)
	writeJSONL(t, dir, "here", cwd, "here prompt", now.Add(-time.Hour))

	chosen := make(chan string, 4)
	var p atomic.Pointer[uiCfg]
	p.Store(mkCfg(t, cur, chosen, &p))
	d := &drv{t: t, cfgp: &p}
	d.m = newModel(80, 24, func(line string) { d.sent = append(d.sent, line) }, nil, &p)
	return d, chosen
}

// entriesOf parses a session JSONL the way the history plugin would.
func entriesOf(t *testing.T, path string) []history.Entry {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var out []history.Entry
	for l := range strings.SplitSeq(strings.TrimSpace(string(b)), "\n") {
		var e history.Entry
		if err := json.Unmarshal([]byte(l), &e); err != nil {
			t.Fatal(err)
		}
		out = append(out, e)
	}
	return out
}

// mkCfg is a cfg on the session at path whose choose seam records the
// id and swaps the cfg to that session (what the launcher's remount
// does).
func mkCfg(t *testing.T, path string, chosen chan string, p *atomic.Pointer[uiCfg]) *uiCfg {
	cfg := cfgWith(t, nil, nil, fakeHist{path: path, entries: entriesOf(t, path)})
	cfg.cmds = sessionsReg(t)
	cfg.choose = func(id string) {
		chosen <- id
		p.Store(mkCfg(t, filepath.Join(filepath.Dir(path), id+".jsonl"), chosen, p))
	}
	return cfg
}

func (d *drv) dispatchLine(line string) {
	d.m.input.SetValue(line)
	d.press(keyEnter())
}

func TestResumedRowNamesSessionAndLastPrompt(t *testing.T) {
	d, _ := midSession(t)
	p := d.plain()
	if !strings.Contains(p, "resumed cur · 3 entries · last: current prompt") {
		t.Fatalf("resumed row missing:\n%s", p)
	}
	if !d.m.vp.AtBottom() {
		t.Error("resume should land on the last exchange")
	}
}

func TestSlashSessionsOpensPickerCwdFirstCurrentMarked(t *testing.T) {
	d, _ := midSession(t)
	d.dispatchLine("/sessions")
	if !d.m.picking {
		t.Fatal("/sessions should open the picker")
	}
	p := d.plain()
	iCur, iHere, iOther := strings.Index(p, "current prompt"), strings.Index(p, "here prompt"), strings.Index(p, "other prompt")
	if iCur < 0 || iHere < 0 || iOther < 0 {
		t.Fatalf("picker rows missing:\n%s", p)
	}
	if !(iCur < iHere && iHere < iOther) {
		t.Errorf("this directory's sessions should come first (cur, here, then other):\n%s", p)
	}
	for l := range strings.SplitSeq(p, "\n") {
		switch {
		case strings.Contains(l, "current prompt"):
			if !strings.Contains(l, "(current)") || !strings.Contains(l, "  .") {
				t.Errorf("current row should be marked and show '.' cwd: %q", l)
			}
		case strings.Contains(l, "other prompt"):
			if !strings.Contains(l, "/elsewhere/project") {
				t.Errorf("foreign row should show its cwd: %q", l)
			}
		}
	}
	if !strings.Contains(p, "esc back") {
		t.Errorf("mid-session hint should say esc goes back:\n%s", p)
	}
}

func TestPickerEscMidSessionKeepsTranscript(t *testing.T) {
	d, chosen := midSession(t)
	d.dispatchLine("/sessions")
	n := len(d.m.blocks) // transcript + the "❯ /sessions" echo
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.picking {
		t.Fatal("esc should close the picker")
	}
	select {
	case id := <-chosen:
		t.Fatalf("esc mid-session must not choose, chose %q", id)
	default:
	}
	if len(d.m.blocks) != n {
		t.Errorf("esc changed the transcript: %d -> %d blocks", n, len(d.m.blocks))
	}
}

func TestPickerEnterMidSessionSwapsAndReplays(t *testing.T) {
	d, chosen := midSession(t)
	d.dispatchLine("/sessions")
	d.press(keyDown())
	d.press(keyDown()) // "other"
	d.press(keyEnter())
	select {
	case id := <-chosen:
		if id != "other" {
			t.Fatalf("chose %q, want other", id)
		}
	default:
		t.Fatal("enter did not choose")
	}
	p := d.plain()
	if d.m.picking || !strings.Contains(p, "❯ other prompt") || strings.Contains(p, "❯ current prompt") {
		t.Fatalf("after resume the transcript should be the other session's:\n%s", p)
	}
	if !strings.Contains(p, "resumed other · 3 entries") {
		t.Errorf("resumed row missing after swap:\n%s", p)
	}
}

func TestPickerEnterOnCurrentIsNoop(t *testing.T) {
	d, chosen := midSession(t)
	d.dispatchLine("/sessions")
	d.press(keyEnter()) // cursor starts on "cur"
	select {
	case id := <-chosen:
		t.Fatalf("resuming the current session should not swap, chose %q", id)
	default:
	}
	if d.m.picking {
		t.Error("picker should close")
	}
}

func TestSlashSessionsIDResumesOrErrors(t *testing.T) {
	d, chosen := midSession(t)
	d.dispatchLine("/sessions nope")
	select {
	case id := <-chosen:
		t.Fatalf("unknown id must not choose, chose %q", id)
	default:
	}
	if p := d.plain(); !strings.Contains(p, `no session "nope"`) {
		t.Fatalf("unknown id should show an error block:\n%s", p)
	}
	d.dispatchLine("/sessions here")
	select {
	case id := <-chosen:
		if id != "here" {
			t.Fatalf("chose %q, want here", id)
		}
	default:
		t.Fatal("/sessions here did not choose")
	}
	if p := d.plain(); !strings.Contains(p, "❯ here prompt") {
		t.Fatalf("transcript should be the resumed session's:\n%s", p)
	}
}

func TestStatusBarClickOpensPicker(t *testing.T) {
	d, _ := midSession(t)
	d.feed(tea.MouseClickMsg{X: 0, Y: d.m.vp.Height(), Button: tea.MouseLeft})
	if !d.m.picking {
		t.Fatal("clicking the status bar should open the picker")
	}
	if p := d.plain(); !strings.Contains(p, "resume a session") {
		t.Fatalf("picker not rendered:\n%s", p)
	}
}

func TestShortDir(t *testing.T) {
	if s := shortDir("", "/c", "/h"); s != "?" {
		t.Errorf("empty = %q", s)
	}
	if s := shortDir("/c", "/c", "/h"); s != "." {
		t.Errorf("cwd = %q", s)
	}
	if s := shortDir("/h/x", "/c", "/h"); s != "~/x" {
		t.Errorf("home = %q", s)
	}
}

// jsonLine encodes one history line. Building JSON by concatenation
// breaks on Windows, where a cwd is C:\\Users\\… and the backslashes
// are read as escapes.
func jsonLine(t *testing.T, v any) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}
