package vtreal

// Bracketed paste through the real PTY: what the composer does with a
// paste, and what actually reaches the loop when it is sent.
//
// The screen can only show what fits, so "the paste arrived intact" is
// asserted against the run's own history file: the "input" entry the
// loop recorded is the exact text the model was given.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// pasteEntries reads every entry bough wrote under this run's $HOME.
func pasteEntries(a *app) []history.Entry {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var all []history.Entry
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		all = append(all, entries...)
	}
	return all
}

// pasteWaitEntry waits for an entry of one of the given kinds whose
// data.text contains want, and returns it.
func pasteWaitEntry(a *app, want string, kinds ...string) history.Entry {
	a.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		for _, e := range pasteEntries(a) {
			text, _ := e.Data["text"].(string)
			for _, k := range kinds {
				if e.Kind == k && strings.Contains(text, want) {
					return e
				}
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.t.Fatalf("no %v entry containing %q was recorded:\n%s", kinds, want, a.text())
	return history.Entry{}
}

// pasteReplayConfig is replayConfig with a per-word stream delay, so a
// turn can be caught mid-flight.
func pasteReplayConfig(tape string, delayMS int) string {
	return strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}", tape, delayMS), 1)
}

// A paste of a few short lines lands in the composer as an editable
// draft: every line is visible, nothing is submitted, and the loop has
// not been given anything.
func TestPasteMultiLineStaysDraft(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	a.term.Paste("alpha one\nbravo two\ncharlie three")
	a.waitFor("charlie three")
	s := a.settled()
	for _, line := range []string{"alpha one", "bravo two", "charlie three"} {
		if !strings.Contains(s, line) {
			t.Fatalf("pasted line %q missing from the draft:\n%s", line, s)
		}
	}
	if strings.Contains(s, "echo:") || strings.Contains(s, "❯ alpha one") {
		t.Fatalf("the paste was submitted without enter:\n%s", s)
	}
	if strings.Contains(s, pastePlaceholder) {
		t.Fatalf("a 3-line paste should stay editable, not collapse:\n%s", s)
	}
	if n := a.doneCount(); n != 0 {
		t.Fatalf("%d turns ran for a paste that was never sent:\n%s", n, a.text())
	}
}

const pastePlaceholder = "[Pasted text #"

// Over 800 characters the paste collapses to a placeholder; enter
// expands it, and the loop receives the full text, head and tail
// intact.
func TestPasteLongCollapsesAndExpands(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	body := strings.Repeat("alpha ", 200) // 1200 chars, one line
	text := "pastehead " + body + "pastetail"
	a.term.Paste(text)
	a.waitFor(fmt.Sprintf("%s1 %d chars]", pastePlaceholder, len(text)))
	s := a.settled()
	if strings.Contains(s, "pastehead alpha alpha") {
		t.Fatalf("the long paste was inserted verbatim instead of collapsing:\n%s", s)
	}
	a.key(uv.KeyEnter, 0)
	e := pasteWaitEntry(a, "pastehead", "input")
	got, _ := e.Data["text"].(string)
	if got != text {
		t.Fatalf("the placeholder expanded to %d chars (want %d); head=%.20q tail=%.20q\n%s",
			len(got), len(text), got, got[max(0, len(got)-20):], a.text())
	}
}

// More lines than the composer can hold collapses too, even well under
// the character threshold, and reports the line count.
func TestPasteManyLinesCollapsesAndExpands(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	var b strings.Builder
	for i := range 12 {
		fmt.Fprintf(&b, "pasteline%02d\n", i)
	}
	text := b.String()
	a.term.Paste(text)
	// The tag counts the lines of the trimmed paste: 12 lines, no
	// trailing blank.
	a.waitFor(pastePlaceholder + "1 +12 lines]")
	s := a.settled()
	if strings.Contains(s, "pasteline05") {
		t.Fatalf("a 12-line paste should collapse, not fill the composer:\n%s", s)
	}
	a.key(uv.KeyEnter, 0)
	e := pasteWaitEntry(a, "pasteline11", "input")
	got, _ := e.Data["text"].(string)
	if !strings.Contains(got, "pasteline00") || !strings.Contains(got, "pasteline11") {
		t.Fatalf("expanded paste lost lines (%q):\n%s", got, a.text())
	}
}

// A paste that is one path to an image on disk becomes the "@path"
// reference, and that is what the model is given.
func TestPasteImagePathBecomesReference(t *testing.T) {
	t.Parallel()
	a := start(t, 200, 24)
	img := filepath.Join(t.TempDir(), "shot.png")
	if err := os.WriteFile(img, []byte("\x89PNG\r\n\x1a\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	a.term.Paste(img)
	a.waitFor("image attached by path")
	if s := a.settled(); !strings.Contains(s, "shot.png") {
		t.Fatalf("the image reference is not in the composer:\n%s", s)
	}
	a.key(uv.KeyEnter, 0)
	e := pasteWaitEntry(a, "shot.png", "input")
	got, _ := e.Data["text"].(string)
	if strings.TrimSpace(got) != "@"+img {
		t.Fatalf("sent %q, want %q:\n%s", got, "@"+img, a.text())
	}
}

// A path that is not an image is an ordinary paste: no @ reference.
func TestPasteNonImagePathIsPlainText(t *testing.T) {
	t.Parallel()
	// Wide enough that the temp path is one unwrapped row.
	a := start(t, 200, 24)
	doc := filepath.Join(t.TempDir(), "notes.txt")
	if err := os.WriteFile(doc, []byte("hi"), 0o644); err != nil {
		t.Fatal(err)
	}
	a.term.Paste(doc)
	a.waitFor(doc)
	if s := a.settled(); strings.Contains(s, "image attached by path") || strings.Contains(s, "@"+doc) {
		t.Fatalf("a .txt path was treated as an image:\n%s", s)
	}
}

// Pasting and sending while a turn is streaming does not start a second
// turn: the ui marks the line as steering the running turn (or queues
// it behind it), and the paste is still expanded in full.
func TestPasteDuringStreamSteersOrQueues(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/paste-stream.jsonl")
	a := startCfg(t, 100, 30, pasteReplayConfig(tape, 60))
	a.typeText("stream please")
	a.key(uv.KeyEnter, 0)
	a.waitFor("streamhead") // the reply is streaming, word by word

	body := strings.Repeat("bravo ", 200) // collapses: 1200 chars
	text := "midstream " + body
	a.term.Paste(text)
	a.waitFor(pastePlaceholder)
	a.key(uv.KeyEnter, 0)

	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "(steer") || strings.Contains(s, "(queued)")
	}, "the mid-turn paste to be marked (steer) or (queued)")
	if s := a.settled(); strings.Count(s, "All done streaming.") > 1 {
		t.Fatalf("the mid-turn paste started a second turn:\n%s", s)
	}
	// Whatever the ui called it, the full text reached the loop.
	e := pasteWaitEntry(a, "midstream", "input", "steer")
	got, _ := e.Data["text"].(string)
	if got != strings.TrimSpace(text) { // the draft is trimmed on submit
		t.Fatalf("mid-turn paste reached the loop as %d chars, want %d:\n%s", len(got), len(strings.TrimSpace(text)), a.text())
	}
	a.check("after mid-turn paste")
}
