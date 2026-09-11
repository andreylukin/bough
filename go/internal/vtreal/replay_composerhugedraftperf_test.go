package vtreal

// A 50k-character multi-line draft, edited in the middle while a
// reply streams: every key must land within a latency budget, the
// virtual cursor must sit on the right cell after each edit, the
// placeholder must expand to exactly the pasted bytes, and what the
// loop records must be byte-for-byte the edited draft.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// composerHugeDraftPerfKeyBudget is generous: parallel -race runs are
// slow, and a draft that re-renders 50k characters per key blows well
// past it.
const composerHugeDraftPerfKeyBudget = 750 * time.Millisecond

// composerHugeDraftPerfBody is a ~50k-char multi-line paste whose head
// and tail are recognisable and whose ends are not whitespace (the
// draft is trimmed on submit).
func composerHugeDraftPerfBody() string {
	var b strings.Builder
	b.WriteString("hugehead")
	for i := 0; b.Len() < 50_000; i++ {
		fmt.Fprintf(&b, "\nline %05d the quick brown fox jumps over the lazy dog", i)
	}
	b.WriteString("\nhugetail")
	return b.String()
}

// composerHugeDraftPerfCursor returns the reverse-video cursor cell in
// the composer rows and the row's text left of it; ok is false when no
// cursor cell is drawn.
func composerHugeDraftPerfCursor(a *app) (left, under string, ok bool) {
	snap := a.term.Snapshot()
	r := composerRow(a.lines())
	if r < 0 {
		return "", "", false
	}
	for y := r; y < len(snap.Cells); y++ {
		for x, c := range snap.Cells[y] {
			if c.Style.Attrs&uv.AttrReverse == 0 {
				continue
			}
			var sb strings.Builder
			for _, lc := range snap.Cells[y][:x] {
				if lc.Width == 0 {
					continue
				}
				if lc.Content == "" {
					sb.WriteByte(' ')
				} else {
					sb.WriteString(lc.Content)
				}
			}
			under = c.Content
			if under == "" {
				under = " "
			}
			return sb.String(), under, true
		}
	}
	return "", "", false
}

// composerHugeDraftPerfOp sends one key and waits until the cursor
// cell shows under with the row left of it ending in leftSuffix; it
// fails when that takes longer than the budget.
func composerHugeDraftPerfOp(a *app, what string, send func(), leftSuffix, under string) time.Duration {
	a.t.Helper()
	t0 := time.Now()
	send()
	for {
		l, u, ok := composerHugeDraftPerfCursor(a)
		if ok && u == under && strings.HasSuffix(l, leftSuffix) {
			d := time.Since(t0)
			if d > composerHugeDraftPerfKeyBudget {
				a.t.Errorf("%s: took %v (budget %v)", what, d, composerHugeDraftPerfKeyBudget)
			}
			return d
		}
		if time.Since(t0) > 10*time.Second {
			a.t.Fatalf("%s: cursor cell never became %q after %q (now %q after %q, drawn=%v):\n%s",
				what, under, leftSuffix, u, l, ok, a.text())
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// The main path: type, paste 50k chars mid-stream, type more, walk
// back over the placeholder with arrows, alt+backspace a word, type,
// send.
func TestComposerHugeDraftPerfPasteEditWhileStreaming(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/paste-stream.jsonl")
	a := startCfg(t, 100, 30, pasteReplayConfig(tape, 150))
	a.typeText("stream please")
	a.key(uv.KeyEnter, 0)
	a.waitFor("streamhead")

	body := composerHugeDraftPerfBody()
	tag := fmt.Sprintf("%s1 +%d lines]", pastePlaceholder, strings.Count(body, "\n")+1)

	a.typeText("alpha bravo charlie ")
	a.waitFor("> alpha bravo charlie")
	t0 := time.Now()
	a.term.Paste(body)
	a.waitFor(tag)
	if d := time.Since(t0); d > 5*time.Second {
		t.Errorf("a %d-char paste took %v to show its placeholder", len(body), d)
	}
	const tail = " delta echo"
	a.typeText(tail)
	a.waitFor(tag + tail)
	if strings.Contains(a.text(), "All done streaming.") {
		t.Fatalf("the reply finished before the edits began; the tape delay is too short:\n%s", a.text())
	}

	var worst time.Duration
	note := func(d time.Duration) { worst = max(worst, d) }
	left := func() { a.key(uv.KeyLeft, 0) }
	// Back over the tail: the cursor ends on the space after the tag.
	for i := range len(tail) {
		n := len(tail) - 1 - i
		note(composerHugeDraftPerfOp(a, fmt.Sprintf("left #%d", i+1), left, tag+tail[:n], string(tail[n])))
	}
	// Back over the placeholder itself: the cursor ends on its "[".
	for i := range len(tag) {
		n := len(tag) - 1 - i
		note(composerHugeDraftPerfOp(a, fmt.Sprintf("left over tag #%d", i+1), left, "charlie "+tag[:n], string(tag[n])))
	}
	note(composerHugeDraftPerfOp(a, "alt+backspace", func() { a.key(uv.KeyBackspace, uv.ModAlt) }, "> alpha bravo ", "["))
	note(composerHugeDraftPerfOp(a, "type X", func() { a.typeText("X") }, "> alpha bravo X", "["))
	t.Logf("worst key latency over a %d-char draft: %v", len(body), worst)
	if s := a.text(); !strings.Contains(s, "> alpha bravo X"+tag+tail) {
		t.Fatalf("the edited draft is not what the keys produced:\n%s", s)
	}

	a.key(uv.KeyEnter, 0)
	want := "alpha bravo X" + body + tail
	e := pasteWaitEntry(a, "hugetail", "input", "steer")
	got, _ := e.Data["text"].(string)
	if got != want {
		i := 0
		for i < min(len(got), len(want)) && got[i] == want[i] {
			i++
		}
		t.Fatalf("sent %d bytes, want %d; first difference at byte %d: got %.40q want %.40q",
			len(got), len(want), i, got[i:], want[i:])
	}
	a.check("after the huge draft was sent")
}

// Soak: 50k characters typed (no paste), then edited and sent. Slow
// by design: BOUGH_SOAK_COMPOSERHUGEDRAFTPERF=1 runs it.
func TestComposerHugeDraftPerfTypedSoak(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_COMPOSERHUGEDRAFTPERF") == "" {
		t.Skip("soak: set BOUGH_SOAK_COMPOSERHUGEDRAFTPERF=1 to type a 50k-char draft")
	}
	if os.Getenv("BOUGH_KNOWN_COMPOSERHUGEDRAFTPERF") == "" {
		t.Skip("known bug: typing a 50k-char draft never finishes in 10 min — bough stops draining the PTY " +
			"(per-key composer cost grows with the draft); set BOUGH_KNOWN_COMPOSERHUGEDRAFTPERF=1 to run")
	}
	t.Parallel()
	a := start(t, 100, 30)
	head := strings.Repeat("typed ", 50_000/len("typed "))
	body := head + "typedtail"
	for i := 0; i < len(body); i += 500 {
		a.typeText(body[i:min(i+500, len(body))])
	}
	a.waitFor("typedtail")
	composerHugeDraftPerfOp(a, "left", func() { a.key(uv.KeyLeft, 0) }, "typedtai", "l")
	composerHugeDraftPerfOp(a, "alt+backspace", func() { a.key(uv.KeyBackspace, uv.ModAlt) }, "typed ", "l")
	a.key(uv.KeyEnter, 0)
	want := head + "l"
	e := pasteWaitEntry(a, "typed", "input")
	if got, _ := e.Data["text"].(string); got != want {
		t.Fatalf("sent %d bytes, want %d (tail %q)", len(got), len(want), got[max(0, len(got)-20):])
	}
}
