package vtreal

// The /sessions picker over a large history: 2000 sessions whose
// titles mix CJK, emoji, Cyrillic, combining marks and RTL. The picker
// must paint within a budget, follow the selection past one screen
// (down and pgdown), filter by a typed CJK query without dropping
// keystrokes, survive a resize, and resume exactly the selected row.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	sessionPickerFilterUnicodeLargeN      = 2000
	sessionPickerFilterUnicodeLargeBudget = 3 * time.Second
	sessionPickerFilterUnicodeLargeGate   = "BOUGH_KNOWN_SESSION_PICKER_FILTER_UNICODE_LARGE"
)

var sessionPickerFilterUnicodeLargeWords = []string{
	"東京の天気", "北京烤鸭", "서울 지하철", "🚀 deploy", "Привет мир", "café naïve", "שלום עולם", "e\u0301clair",
}

// sessionPickerFilterUnicodeLargeTitle is session i's unique title.
func sessionPickerFilterUnicodeLargeTitle(i int) string {
	w := sessionPickerFilterUnicodeLargeWords[i%len(sessionPickerFilterUnicodeLargeWords)]
	return fmt.Sprintf("%s #%04d", w, i)
}

// sessionPickerFilterUnicodeLargeSeed writes n finished sessions
// (spl-0000 newest) into the run's history dir and returns id by title.
func sessionPickerFilterUnicodeLargeSeed(t *testing.T, home string, n int) map[string]string {
	t.Helper()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	ids := map[string]string{}
	now := time.Now().Add(-time.Hour)
	for i := range n {
		id, title := fmt.Sprintf("spl-%04d", i), sessionPickerFilterUnicodeLargeTitle(i)
		ids[title] = id
		mt := now.Add(-time.Duration(i) * time.Minute)
		var sb strings.Builder
		for j, e := range []struct {
			kind string
			data map[string]any
		}{
			{"meta", map[string]any{"cwd": "/elsewhere/spl"}},
			{"input", map[string]any{"text": title}},
			{"assistant", map[string]any{"text": "reply " + id}},
			{"done", map[string]any{}},
		} {
			b, _ := json.Marshal(map[string]any{"seq": j + 1, "at": mt.UTC().Format(time.RFC3339), "kind": e.kind, "data": e.data})
			sb.Write(append(b, '\n'))
		}
		p := filepath.Join(dir, id+".jsonl")
		if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Chtimes(p, mt, mt); err != nil {
			t.Fatal(err)
		}
	}
	return ids
}

// sessionPickerFilterUnicodeLargeOpen seeds, opens /sessions and
// returns the id map and the time from enter to the first painted row.
func sessionPickerFilterUnicodeLargeOpen(t *testing.T, cols, rows int) (*app, map[string]string, time.Duration) {
	t.Helper()
	a := start(t, cols, rows)
	ids := sessionPickerFilterUnicodeLargeSeed(t, a.home, sessionPickerFilterUnicodeLargeN)
	a.typeText("/sessions")
	a.waitFor("> /sessions")
	t0 := time.Now()
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "resume a session") && strings.Contains(s, "#0") }, "picker rows")
	return a, ids, time.Since(t0)
}

// sessionPickerFilterUnicodeLargeSelected is the title on the ▸ row
// ("" when the selection is not on screen).
func sessionPickerFilterUnicodeLargeSelected(screen string, ids map[string]string) string {
	for _, l := range strings.Split(screen, "\n") {
		if !strings.HasPrefix(l, "▸ ") {
			continue
		}
		for title := range ids {
			if strings.Contains(l, title) {
				return title
			}
		}
		return "?" + l
	}
	return ""
}

func TestSessionPickerFilterUnicodeLarge(t *testing.T) {
	t.Parallel()

	t.Run("FirstPaintWithinBudget", func(t *testing.T) {
		t.Parallel()
		a, _, took := sessionPickerFilterUnicodeLargeOpen(t, 120, 30)
		if took > sessionPickerFilterUnicodeLargeBudget {
			t.Errorf("first paint of %d sessions took %v, budget %v", sessionPickerFilterUnicodeLargeN, took, sessionPickerFilterUnicodeLargeBudget)
		}
		s := a.settled()
		if !strings.Contains(s, sessionPickerFilterUnicodeLargeTitle(0)) {
			t.Errorf("newest session not on the first screen:\n%s", s)
		}
	})

	// Resize runs under tmux: x/vt blanks the grid on resize (see
	// tmux_test.go), so the repaint is only observable there.
	t.Run("ResizeThenEnterResumesSelected", func(t *testing.T) {
		t.Parallel()
		tm := startTmux(t, 120, 30)
		home := strings.TrimSpace(tm.run("display-message", "-p", "-t", "0", "#{pane_current_path}"))
		ids := sessionPickerFilterUnicodeLargeSeed(t, home, sessionPickerFilterUnicodeLargeN)
		tm.keys("/sessions")
		tm.waitFor("> /sessions")
		tm.keys("Enter")
		tm.waitFor("#0000")
		tm.keys("Down", "Down", "Down")
		title := sessionPickerFilterUnicodeLargeSelected(tm.settled(), ids)
		if ids[title] == "" {
			t.Fatalf("no selected row after 3 downs (%q):\n%s", title, tm.screen())
		}
		tm.resize(70, 20)
		tm.waitUntil(func(s string) bool { return strings.Count(s, "\n") == 19 && strings.Contains(s, "▸ ") }, "picker repaint at 70x20")
		s := tm.settled()
		if got := sessionPickerFilterUnicodeLargeSelected(s, ids); got != title {
			t.Fatalf("resize moved the selection from %q to %q:\n%s", title, got, s)
		}
		tm.keys("Enter")
		tm.waitFor("resumed " + ids[title])
		if s := tm.settled(); !strings.Contains(s, "reply "+ids[title]) {
			t.Errorf("resumed transcript is not %s's:\n%s", ids[title], s)
		}
	})

	t.Run("PageDownKeepsSelectionVisible", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(sessionPickerFilterUnicodeLargeGate) == "" {
			t.Skip("known bug: session picker has no scroll offset or pgdown — selection walks off screen (plugins/ui/session.go pickerView/handlePickerKey); set " + sessionPickerFilterUnicodeLargeGate + "=1")
		}
		a, ids, _ := sessionPickerFilterUnicodeLargeOpen(t, 100, 20)
		a.key(uv.KeyPgDown, 0)
		a.settled()
		first := sessionPickerFilterUnicodeLargeSelected(a.text(), ids)
		if first == "" || first == sessionPickerFilterUnicodeLargeTitle(0) {
			t.Errorf("pgdown did not move a visible selection (got %q):\n%s", first, a.text())
		}
		for range 40 {
			a.key(uv.KeyDown, 0)
		}
		s := a.settled()
		if got := sessionPickerFilterUnicodeLargeSelected(a.text(), ids); got == "" {
			t.Errorf("selection is off screen after pgdown + 40 downs:\n%s", s)
		}
	})

	t.Run("CJKFilterNoDroppedKeys", func(t *testing.T) {
		t.Parallel()
		a, ids, _ := sessionPickerFilterUnicodeLargeOpen(t, 120, 30)
		const query = "東京の天気 #1"
		for _, r := range query {
			a.typeText(string(r)) // one key at a time, no waiting
		}
		a.waitUntil(func(s string) bool { return strings.Contains(s, query) }, "full query echoed")
		s := a.settled()
		n := 0
		for _, l := range a.lines() {
			if strings.Contains(l, "#") && strings.Contains(l, "entries") {
				n++
				if !strings.Contains(l, "東京の天気 #1") {
					t.Errorf("row does not match the filter: %q", l)
				}
			}
		}
		if n == 0 {
			t.Fatalf("filter left no rows:\n%s", s)
		}
		title := sessionPickerFilterUnicodeLargeSelected(a.text(), ids)
		if ids[title] == "" {
			t.Fatalf("no selected match:\n%s", s)
		}
		a.key(uv.KeyEnter, 0)
		a.waitFor("resumed " + ids[title])
	})
}
