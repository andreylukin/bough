package vtreal

// A bracketed paste as the freeform answer to a pending tools.ask, on
// the ask tape (testdata/replay/ask.jsonl: ask, then the model's echo
// reply). The paste must stay a draft, collapse to its placeholder,
// and on enter the "ask/answer" entry in the run's history must be
// the full pasted text, not the placeholder.

import (
	"fmt"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// askPasteAnswerText is 5 lines and exactly 1000 characters, with no
// leading or trailing whitespace for the composer to trim.
func askPasteAnswerText() string {
	lines := make([]string, 5)
	for i := range lines {
		head := fmt.Sprintf("pasteans%d ", i)
		// 5 lines of 199 chars + 4 newlines = 999; pad the last by one.
		n := 199 - len(head)
		if i == 4 {
			n++
		}
		lines[i] = head + strings.Repeat("x", n)
	}
	return strings.Join(lines, "\n")
}

// askPasteAnswerEntry waits for the "ask/answer" history entry.
func askPasteAnswerEntry(a *app) string {
	a.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		for _, e := range pasteEntries(a) {
			if e.Kind == "ask/answer" {
				text, _ := e.Data["text"].(string)
				return text
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.t.Fatalf("no ask/answer entry was recorded:\n%s", a.text())
	return ""
}

func TestAskPasteAnswer(t *testing.T) {
	t.Parallel()
	text := askPasteAnswerText()
	if len(text) != 1000 || strings.Count(text, "\n") != 4 {
		t.Fatalf("bad fixture: %d chars, %d newlines", len(text), strings.Count(text, "\n"))
	}
	a := askStart(t)
	a.settled()
	a.term.Paste(text)
	// A multi-line paste is tagged by its line count (plugins/ui/paste.go).
	a.waitFor(pastePlaceholder + "1 +5 lines]")
	s := a.settled()
	if strings.Contains(s, "pasteans0 xxxx") {
		t.Fatalf("the long paste was inserted verbatim instead of collapsing:\n%s", s)
	}
	if strings.Contains(s, "Color locked in.") || !strings.Contains(s, "? Pick a color") {
		t.Fatalf("the paste answered the ask without enter:\n%s", s)
	}
	for _, e := range pasteEntries(a) {
		if e.Kind == "ask/answer" {
			t.Fatalf("ask/answer recorded before enter: %v", e.Data)
		}
	}
	a.key(uv.KeyEnter, 0)
	got := askPasteAnswerEntry(a)
	if got != text {
		t.Fatalf("ask/answer is %d chars (want %d): %.60q\n%s", len(got), len(text), got, a.text())
	}
	a.waitFor("Color locked in.")
}
