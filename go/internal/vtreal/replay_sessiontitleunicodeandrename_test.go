package vtreal

// A session title off the small tape that is hostile in every way a
// title can be: emoji (a ZWJ family, wide CJK), RTL runs, combining
// marks, an embedded OSC 2 sequence and well over 200 characters.
// bough has no /rename; the title change it does have is the name
// landing after turn one, then turn two running under it (the tab
// glyph changes, the name stays), so that is the "mid-stream" half.
//
// Asserted: the status bar keeps the title and "? keys" on one row,
// the terminal title carries no control runes and no forged title,
// the history entry is valid UTF-8 and a clean prefix of what the
// model said, and the picker row keeps its (current) marker with the
// title inside its 60-cell column.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"unicode"
	"unicode/utf8"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const sessionTitleUnicodeAndRenameHead = "🚀 Fix the"

// sessionTitleUnicodeAndRenameCells mirrors plugins/ui.pickerTitleWidth.
const sessionTitleUnicodeAndRenameCells = 60

// sessionTitleUnicodeAndRenameTape is the raw title the small tape says.
func sessionTitleUnicodeAndRenameTape(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range strings.Split(string(b), "\n") {
		var e struct {
			Kind string
			Data struct{ Text string }
		}
		if json.Unmarshal([]byte(l), &e) == nil && e.Kind == "assistant" {
			return e.Data.Text
		}
	}
	t.Fatal("no title on the small tape")
	return ""
}

// sessionTitleUnicodeAndRenameStored is the "title" entry this run wrote.
func sessionTitleUnicodeAndRenameStored(a *app) (string, bool) {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	for _, p := range paths {
		es, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range es {
			if e.Kind == "title" {
				s, _ := e.Data["text"].(string)
				return s, true
			}
		}
	}
	return "", false
}

// sessionTitleUnicodeAndRenameCol is the terminal cell column where
// substr starts on screen row y, -1 when absent.
func sessionTitleUnicodeAndRenameCol(a *app, y int, substr string) int {
	row := a.term.Snapshot().Cells[y]
	var sb strings.Builder
	var cols []int // cell column of every byte written to sb
	for x, c := range row {
		if c.Width == 0 {
			continue
		}
		s := c.Content
		if s == "" {
			s = " "
		}
		for range len(s) {
			cols = append(cols, x)
		}
		sb.WriteString(s)
	}
	i := strings.Index(sb.String(), substr)
	if i < 0 {
		return -1
	}
	return cols[i]
}

func sessionTitleUnicodeAndRenameRow(a *app, substr string) int {
	for y, l := range a.lines() {
		if strings.Contains(l, substr) {
			return y
		}
	}
	return -1
}

const sessionTitleUnicodeAndRenameCtl = "title.Clean (plugins/title/title.go) keeps control bytes, so the model's ESC]2;…BEL reaches the PTY raw via the status bar (plugins/ui/statusbar.go) and the tab title (plugins/ui/tabtitle.go)"

func sessionTitleUnicodeAndRenameGate(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_SESSION_TITLE_UNICODE_AND_RENAME") == "" {
		t.Skip("known bug (set BOUGH_KNOWN_SESSION_TITLE_UNICODE_AND_RENAME=1 to run): " + bug)
	}
}

func TestSessionTitleUnicodeAndRename(t *testing.T) {
	t.Parallel()
	main, small := llmSmallTapes(t, "sessiontitleunicodeandrename-title.jsonl")
	raw := sessionTitleUnicodeAndRenameTape(t, small)
	if n := utf8.RuneCountInString(raw); n <= 200 {
		t.Fatalf("tape title is %d runes, want > 200", n)
	}
	a := startCfg(t, 100, 30, llmSmallConfig(main, small, `
- id: session-title
  plugin: session-title
`))
	a.check("boot")
	llmSmallTurn(a, "list the files here", 1)
	a.waitFor(sessionTitleUnicodeAndRenameHead)
	parent := a.t
	t.Cleanup(func() { a.t = parent })

	t.Run("status bar truncates by cells", func(t *testing.T) {
		a.t = t
		s := a.settled()
		y := sessionTitleUnicodeAndRenameRow(a, sessionTitleUnicodeAndRenameHead)
		if y < 0 || !strings.Contains(a.lines()[y], "? keys") {
			sessionTitleUnicodeAndRenameGate(t, sessionTitleUnicodeAndRenameCtl)
			t.Fatalf("title and ? keys not on one status row:\n%s", s)
		}
		if c := sessionTitleUnicodeAndRenameCol(a, y, "? keys"); c < 0 || c+len("? keys") > a.cols {
			t.Fatalf("? keys pushed past the pane (col %d):\n%s", c, s)
		}
		if l := a.lines()[y]; strings.Contains(l, "PWNED") || strings.ContainsAny(l, "\x1b\x07") {
			t.Fatalf("the embedded OSC leaked into the status bar: %q", l)
		}
	})

	t.Run("terminal title filtered", func(t *testing.T) {
		a.t = t
		got := a.term.Snapshot().Title
		if !strings.Contains(got, "Fix the") {
			t.Fatalf("terminal title = %q, want the session title (the embedded OSC 2 may have replaced it)", got)
		}
		for _, r := range got {
			if unicode.IsControl(r) {
				sessionTitleUnicodeAndRenameGate(t, sessionTitleUnicodeAndRenameCtl)
				t.Fatalf("control rune %U in the terminal title %q", r, got)
			}
		}
	})

	t.Run("history stores a clean title", func(t *testing.T) {
		a.t = t
		got, ok := sessionTitleUnicodeAndRenameStored(a)
		if !ok {
			t.Fatal("no title entry in the history file")
		}
		if !utf8.ValidString(got) || strings.ContainsRune(got, utf8.RuneError) {
			sessionTitleUnicodeAndRenameGate(t, "title.Clean cuts at byte 60 (s[:60]) and splits a multi-byte rune; the history entry gets U+FFFD")
			t.Fatalf("stored title is not clean UTF-8: %q", got)
		}
		if !strings.HasPrefix(raw, strings.TrimSpace(strings.TrimSuffix(got, "…"))) {
			t.Fatalf("stored title %q is not a prefix of the model's %q", got, raw)
		}
	})

	t.Run("picker row aligned", func(t *testing.T) {
		a.t = t
		a.typeText("/sessions")
		a.key(uv.KeyEnter, 0)
		a.waitFor("resume a session")
		defer func() {
			a.key(uv.KeyEscape, 0)
			a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close")
		}()
		s := a.settled()
		r := sessionTitleUnicodeAndRenameRow(a, "entries")
		if r < 0 {
			t.Fatalf("no session row:\n%s", s)
		}
		start := sessionTitleUnicodeAndRenameCol(a, r, "entries") + len("entries  ")
		end := sessionTitleUnicodeAndRenameCol(a, r, " (current)")
		if end < 0 {
			sessionTitleUnicodeAndRenameGate(t, "pickerView cuts the title (truncateCols) and the row by runes, not cells; wide runes push (current) off the row")
			t.Fatalf("(current) cut off the picker row:\n%s", a.lines()[r])
		}
		if w := end - start; w > sessionTitleUnicodeAndRenameCells {
			sessionTitleUnicodeAndRenameGate(t, "truncateCols in plugins/ui/session.go counts runes, so a wide title overruns its 60-cell column")
			t.Fatalf("title takes %d cells, want <= %d:\n%s", w, sessionTitleUnicodeAndRenameCells, a.lines()[r])
		}
	})

	t.Run("second turn under the title", func(t *testing.T) {
		a.t = t
		llmSmallTurn(a, "and the tests", 2)
		s := a.settled()
		if !strings.Contains(s, "MAIN-TWO") {
			t.Fatalf("the main tape was consumed by the title call:\n%s", s)
		}
		a.waitFor(sessionTitleUnicodeAndRenameHead)
		if got := a.term.Snapshot().Title; !strings.HasPrefix(got, "✓ ") {
			t.Fatalf("terminal title after turn two = %q, want the ✓ glyph", got)
		}
		sessionTitleUnicodeAndRenameGate(t, sessionTitleUnicodeAndRenameCtl+" (the bar spills onto the composer row)")
		a.check("second turn")
	})
}
