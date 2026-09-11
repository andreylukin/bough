package vtreal

// Resize while choosing where a session runs: the /sessions picker,
// which lists each session's cwd, and the "/new <dir>" dialog with
// fzf directory autocomplete (plugins/ui/newdir.go) are each resized
// 120x40 → 40x12 → 200x50 with a selection held.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/plugins/history"
)

// resizeDuringNewSessionDialogFzfHome is the run's $HOME (startTmux
// cds there before exec).
func resizeDuringNewSessionDialogFzfHome(tm *tmuxApp) string {
	return strings.TrimSpace(tm.run("display-message", "-p", "-t", "0", "#{pane_current_path}"))
}

// resizeDuringNewSessionDialogFzfSelected is the picker's selected row.
func resizeDuringNewSessionDialogFzfSelected(s string) string {
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "▸ ") {
			return l
		}
	}
	return ""
}

// resizeDuringNewSessionDialogFzfBounds fails on any row wider than cols
// or a screen taller than rows.
func resizeDuringNewSessionDialogFzfBounds(t *testing.T, s string, cols, rows int) {
	t.Helper()
	ls := strings.Split(s, "\n")
	if len(ls) > rows {
		t.Errorf("%dx%d: %d rows on screen:\n%s", cols, rows, len(ls), s)
	}
	for i, l := range ls {
		if n := utf8.RuneCountInString(l); n > cols {
			t.Errorf("%dx%d: row %d is %d cells: %q", cols, rows, i, n, l)
		}
	}
}

func TestResizeDuringNewSessionDialogFzf(t *testing.T) {
	t.Parallel()

	t.Run("SessionsPickerCwdSurvivesResize", func(t *testing.T) {
		t.Parallel()
		tm := startTmux(t, 120, 40)
		home := resizeDuringNewSessionDialogFzfHome(tm)
		a := &app{t: t, home: home}
		for _, n := range []string{"alpha", "beta", "gamma"} {
			dir := filepath.Join(home, "tree", n)
			if err := os.MkdirAll(dir, 0o755); err != nil {
				t.Fatal(err)
			}
			newSessionSeed(t, a, "seed-"+n, dir, n+" prompt")
		}
		tm.keys("-l", "/sessions")
		tm.waitFor("> /sessions")
		tm.keys("Enter")
		tm.waitFor("resume a session")
		for i := 0; i < 5 && !strings.Contains(resizeDuringNewSessionDialogFzfSelected(tm.settled()), "beta prompt"); i++ {
			tm.keys("Down")
		}
		if sel := resizeDuringNewSessionDialogFzfSelected(tm.settled()); !strings.Contains(sel, "beta prompt") {
			t.Fatalf("could not select beta with its cwd:\n%s", tm.screen())
		}

		for _, sz := range [][2]int{{40, 12}, {200, 50}} {
			tm.resize(sz[0], sz[1])
			s := tm.settled()
			resizeDuringNewSessionDialogFzfBounds(t, s, sz[0], sz[1])
			if resizeDuringNewSessionDialogFzfSelected(s) == "" {
				t.Errorf("%dx%d: no selected row:\n%s", sz[0], sz[1], s)
			}
		}
		s := tm.settled()
		if sel := resizeDuringNewSessionDialogFzfSelected(s); !strings.Contains(sel, "beta prompt") {
			t.Fatalf("selection lost across resizes, selected %q:\n%s", sel, s)
		}

		tm.keys("Enter")
		tm.waitFor("resumed seed-beta")
		if s := tm.settled(); !strings.Contains(s, "reply to beta prompt") || strings.Contains(s, "gamma prompt") {
			t.Errorf("resumed transcript is not beta's:\n%s", s)
		}
		es, err := history.Read(filepath.Join(home, ".bough", "history", "seed-beta.jsonl"))
		if err != nil || len(es) == 0 || es[0].Kind != "meta" {
			t.Fatalf("seed-beta history unreadable: %v %v", es, err)
		}
		if cwd, _ := es[0].Data["cwd"].(string); cwd != filepath.Join(home, "tree", "beta") {
			t.Errorf("resumed session cwd = %q, want %q", cwd, filepath.Join(home, "tree", "beta"))
		}
	})

	t.Run("DialogFzfCentered", func(t *testing.T) {
		t.Parallel()
		tm := startTmux(t, 120, 40)
		home := resizeDuringNewSessionDialogFzfHome(tm)
		for _, n := range []string{"proj-alpha", "proj-beta"} {
			if err := os.MkdirAll(filepath.Join(home, n), 0o755); err != nil {
				t.Fatal(err)
			}
		}
		tm.keys("-l", "/new ")
		tm.keys("-l", "proj-")
		tm.waitFor("proj-beta")
		tm.keys("Down")
		for _, sz := range [][2]int{{40, 12}, {200, 50}} {
			tm.resize(sz[0], sz[1])
			s := tm.settled()
			resizeDuringNewSessionDialogFzfBounds(t, s, sz[0], sz[1])
			if !strings.Contains(s, "╭") || !strings.Contains(s, "╯") {
				t.Errorf("%dx%d: dialog border corners missing:\n%s", sz[0], sz[1], s)
			}
		}
		if !strings.Contains(resizeDuringNewSessionDialogFzfSelected(tm.settled()), "proj-beta") {
			t.Errorf("selection lost:\n%s", tm.screen())
		}

		// Enter starts the session in the picked directory.
		tm.keys("Enter")
		want := filepath.Join(home, "proj-beta")
		for deadline := time.Now().Add(10 * time.Second); ; {
			paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
			for _, p := range paths {
				if es, err := history.Read(p); err == nil && len(es) > 0 && es[0].Kind == "meta" && es[0].Data["cwd"] == want {
					return
				}
			}
			if time.Now().After(deadline) {
				t.Fatalf("no session file with cwd %q after enter:\n%s", want, tm.screen())
			}
			time.Sleep(100 * time.Millisecond)
		}
	})
}
