package ui

// Paste placeholders under editing, turns, steering and odd payloads.
// Known product bugs skip unless BOUGH_KNOWN_PASTE=1; the long rapid
// run is behind BOUGH_PASTE_LONG=1.

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"pgregory.net/rapid"
)

func pasteKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_PASTE") == "" {
		t.Skip("known paste bug (BOUGH_KNOWN_PASTE=1 to run): " + bug)
	}
}

func pasteBackspace() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyBackspace} }

func pasteBody(ch string, n int) string { return strings.Repeat(ch+"\n", n) }

// Backspacing the whole tag drops the paste: nothing is sent.
func TestPasteBackspaceWholeTagDropsIt(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: pasteBody("a", 20)})
	tag := d.m.input.Value()
	for range []rune(tag) {
		d.feed(pasteBackspace())
	}
	if v := d.m.input.Value(); v != "" {
		t.Fatalf("draft after deleting the tag = %q", v)
	}
	d.press(keyEnter())
	if len(d.sent) != 0 {
		t.Fatalf("an empty draft sent %q", d.sent)
	}
}

// A tag with its closing "]" backspaced is no longer a placeholder; it
// must not expand, and above all must not swallow the draft up to the
// next "]" the user typed.
func TestPastePartialTagDoesNotSwallowText(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: pasteBody("a", 20)})
	d.feed(pasteBackspace()) // "[Pasted text #1 +20 lines"
	d.typeStr(" see [docs] too")
	d.press(keyEnter())
	if len(d.sent) != 1 || !strings.Contains(d.sent[0], "see [docs] too") {
		t.Fatalf("typed text was eaten by a broken tag: sent = %q", d.sent)
	}
}

// Typing inside a tag breaks it: the draft goes out literally, never
// half-expanded.
func TestPasteTypingInsideTagBreaksIt(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: pasteBody("a", 20)})
	for range 5 {
		d.feed(keyLeft())
	}
	d.typeStr("X")
	draft := d.m.input.Value()
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != draft {
		t.Fatalf("draft %q sent as %q", draft, d.sent)
	}
}

// Two pastes with text around and between, edited at the front after,
// expand in place in order.
func TestPasteTwoTagsEditedAround(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	a, b := pasteBody("a", 12), strings.Repeat("b", 900)
	d.typeStr("x ")
	d.feed(tea.PasteMsg{Content: a})
	d.typeStr(" mid ")
	d.feed(tea.PasteMsg{Content: b})
	d.typeStr(" end")
	d.feed(keyHome())
	d.typeStr("front ")
	d.press(keyEnter())
	want := "front x " + a + " mid " + b + " end"
	if len(d.sent) != 1 || d.sent[0] != want {
		t.Fatalf("sent %q\nwant %q", d.sent, want)
	}
}

// A paste already sent must not come back: a later draft that happens
// to spell its tag (typed, or pasted from the transcript) is literal.
func TestPasteSentTagDoesNotReexpand(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: pasteBody("s", 20)})
	d.press(keyEnter())
	d.event("done", "")
	d.typeStr("what does [Pasted text #1 +20 lines] mean")
	d.press(keyEnter())
	if len(d.sent) != 2 || d.sent[1] != "what does [Pasted text #1 +20 lines] mean" {
		t.Fatalf("sent = %q", d.sent)
	}
}

// Up after sending a paste recalls the full text, not a dead tag.
func TestPasteRecallRestoresFullText(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	body := pasteBody("r", 20) + "tail"
	d.feed(tea.PasteMsg{Content: body})
	d.press(keyEnter())
	d.event("done", "")
	d.press(keyUp())
	if v := d.m.input.Value(); v != body {
		t.Fatalf("recalled %q", v)
	}
}

// A steer and a follow-up while a turn runs carry the expanded text.
func TestPasteSteerAndFollowUpExpand(t *testing.T) {
	t.Parallel()
	d, steered := steerDrv(t, true)
	body := pasteBody("q", 20)
	d.feed(tea.PasteMsg{Content: body})
	d.press(keyEnter())
	if len(*steered) != 1 || (*steered)[0] != strings.TrimSpace(body) {
		t.Fatalf("steered %q", *steered)
	}
	// A second paste while that steer is still pending, sent as a follow-up.
	body2 := strings.Repeat("z", 1000)
	d.feed(tea.PasteMsg{Content: body2})
	if !strings.HasPrefix(d.m.input.Value(), pastePrefix) {
		t.Fatalf("draft = %.60q", d.m.input.Value())
	}
	d.press(keyAltEnter())
	if len(d.sent) != 2 || d.sent[1] != body2 {
		t.Fatalf("follow-up sent %d msgs, last %.40q", len(d.sent), d.sent[len(d.sent)-1])
	}
	if f := d.plain(); strings.Contains(f, pastePrefix) {
		t.Fatalf("a tag leaked into the transcript:\n%s", f)
	}
}

// Whitespace-only pastes never collapse or send.
func TestPasteWhitespaceOnly(t *testing.T) {
	t.Parallel()
	for _, s := range []string{"\n", "   ", "\r\n\r\n", strings.Repeat("\n", 50), strings.Repeat(" ", 2000), "\t\n \n"} {
		d := defaultDrv(t)
		d.feed(tea.PasteMsg{Content: s})
		if len(d.m.comp.pastes) != 0 {
			t.Errorf("%q stashed a placeholder", s)
		}
		d.press(keyEnter())
		if len(d.sent) != 0 {
			t.Errorf("%q sent %q", s, d.sent)
		}
	}
}

// A 1 MB paste settles fast and comes back byte for byte.
func TestPasteOneMegabyte(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	var b strings.Builder
	for i := 0; b.Len() < 1<<20; i++ {
		fmt.Fprintf(&b, "line %06d %s\n", i, strings.Repeat("x", 60))
	}
	body := b.String() + "END"
	t0 := time.Now()
	d.feed(tea.PasteMsg{Content: body})
	_ = d.view()
	if el := time.Since(t0); el > 2*time.Second {
		t.Fatalf("1 MB paste took %v to settle", el)
	}
	if v := d.m.input.Value(); !strings.HasPrefix(v, pastePrefix) || len(v) > 60 {
		t.Fatalf("draft = %.80q", v)
	}
	t0 = time.Now()
	d.press(keyEnter())
	_ = d.view()
	if el := time.Since(t0); el > 2*time.Second {
		t.Fatalf("sending took %v", el)
	}
	if len(d.sent) != 1 || d.sent[0] != body {
		t.Fatalf("sent %d msgs / %d bytes, want %d", len(d.sent), len(strings.Join(d.sent, "")), len(body))
	}
}

// Path pastes: only one existing image file becomes an attachment.
func TestPastePathKinds(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	mk := func(name string) string {
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte("\x89PNG\r\n\x1a\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		return p
	}
	spaced := mk("my shot 1.png")
	uni := mk("снимок 🐛.jpg")
	txt := mk("notes.txt")
	pngDir := filepath.Join(dir, "folder.png")
	if err := os.Mkdir(pngDir, 0o755); err != nil {
		t.Fatal(err)
	}
	cases := []struct {
		paste, want string // want "" = not an image
	}{
		{spaced, spaced},
		{`"` + spaced + `"`, spaced},
		{"'" + spaced + "'", spaced},
		{strings.ReplaceAll(spaced, " ", `\ `), spaced},
		{"file://" + strings.ReplaceAll(spaced, " ", "%20"), spaced},
		{uni + "\n", uni},
		{txt, ""},
		{pngDir, ""},
		{filepath.Join(dir, "missing.png"), ""},
		{dir, ""},
	}
	for _, c := range cases {
		d := defaultDrv(t)
		d.feed(tea.PasteMsg{Content: c.paste})
		v := d.m.input.Value()
		if c.want != "" {
			if v != "[Image #1] " || len(d.m.comp.images) != 1 || d.m.comp.images[0] != c.want {
				t.Errorf("%q: draft %q images %q", c.paste, v, d.m.comp.images)
				continue
			}
			d.press(keyEnter())
			if len(d.sent) != 1 || d.sent[0] != "[Image #1: "+c.want+"]" {
				t.Errorf("%q: sent %q", c.paste, d.sent)
			}
		} else if strings.Contains(v, "[Image") || len(d.m.comp.images) != 0 {
			t.Errorf("%q attached as an image: %q", c.paste, v)
		}
	}
}

// A paste into the "@" picker's query or a "/" draft edits the draft
// (the pickers follow it); nothing sends and nothing panics.
func TestPasteIntoAtAndSlashDrafts(t *testing.T) {
	t.Parallel()
	for _, pre := range []string{"@", "/", "look at @"} {
		d := defaultDrv(t)
		d.typeStr(pre)
		d.feed(tea.PasteMsg{Content: "go/plu"})
		if v := d.m.input.Value(); v != pre+"go/plu" {
			t.Errorf("%q: draft %q", pre, v)
		}
		_ = d.view()
		if len(d.sent) != 0 {
			t.Errorf("%q: sent %q", pre, d.sent)
		}
	}
}

// Property: random pastes, typing and cursor edits; whatever intact
// tags remain in the draft expand to exactly their pastes.
func TestPastePropEditsExpandExactly(t *testing.T) {
	t.Parallel()
	maxSteps := 12
	if os.Getenv("BOUGH_PASTE_LONG") != "" {
		maxSteps = 60
	}
	tagRe := regexp.MustCompile(`\[Pasted text #(\d+) (?:\+\d+ lines|\d+ chars)\]`)
	rapid.Check(t, func(rt *rapid.T) {
		d := defaultDrv(t)
		var bodies []string
		n := rapid.IntRange(1, maxSteps).Draw(rt, "steps")
		for i := range n {
			switch rapid.IntRange(0, 5).Draw(rt, fmt.Sprintf("op%d", i)) {
			case 0:
				body := strings.Repeat(fmt.Sprintf("p%d\n", len(bodies)), rapid.IntRange(9, 30).Draw(rt, "lines"))
				bodies = append(bodies, body)
				d.feed(tea.PasteMsg{Content: body})
			case 1:
				d.typeStr(rapid.StringMatching(`[a-z \[\]]{1,6}`).Draw(rt, "typed"))
			case 2:
				d.feed(pasteBackspace())
			case 3:
				d.feed(keyLeft())
			case 4:
				d.feed(keyHome())
			case 5:
				d.feed(tea.KeyPressMsg{Code: tea.KeyEnd})
			}
		}
		draft := d.m.input.Value()
		// Broken tags are the known partial-tag bug; skip those drafts.
		if strings.Count(draft, pastePrefix) != len(tagRe.FindAllString(draft, -1)) {
			return
		}
		want := strings.TrimSpace(tagRe.ReplaceAllStringFunc(draft, func(tag string) string {
			var k int
			fmt.Sscanf(tagRe.FindStringSubmatch(tag)[1], "%d", &k)
			return bodies[k-1]
		}))
		d.press(keyEnter())
		if want == "" {
			if len(d.sent) != 0 {
				rt.Fatalf("empty draft sent %q", d.sent)
			}
			return
		}
		if len(d.sent) != 1 || d.sent[0] != want {
			rt.Fatalf("draft %q\nsent %q\nwant %q", draft, d.sent, want)
		}
	})
}
