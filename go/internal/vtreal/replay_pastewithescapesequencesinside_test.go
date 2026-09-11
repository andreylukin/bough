package vtreal

// Bracketed pastes whose payload carries live escape sequences: an SGR
// colour (ESC[31m), an OSC title set (ESC]0;… BEL) and a literal
// "[201~" fragment next to a real terminator split across writes. The
// composer must neutralize them (no colour on the pasted cells, tab
// title unchanged), the paste must end only on the real ESC[201~, and
// the submitted prompt must equal what the composer showed, with no
// ESC byte left in it.

import (
	"image/color"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// pastewithescapesequencesinsideFg is the foreground of the first cell
// of the first occurrence of word on screen, and whether it was found.
func pastewithescapesequencesinsideFg(snap Snapshot, word string) (color.Color, bool) {
	for _, row := range snap.Cells {
		var line strings.Builder
		var idx []int
		for _, c := range row {
			idx = append(idx, line.Len())
			line.WriteString(c.Content)
		}
		at := strings.Index(line.String(), word)
		if at < 0 {
			continue
		}
		for i, off := range idx {
			if off == at {
				return row[i].Style.Fg, true
			}
		}
	}
	return nil, false
}

func TestPasteWithEscapeSequencesInside(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		parts []string
		want  []string // substrings the composer must show, in order
		bad   []string // substrings that must never reach the screen
	}{
		{"sgr_red", []string{"\x1b[200~plainAA \x1b[31mredBB\x1b[0m tailCC\x1b[201~"}, []string{"plainAA", "redBB", "tailCC"}, []string{"[31m", "[0m", "\x1b"}},
		{"osc_title", []string{"\x1b[200~ttA \x1b]0;pwnedtitle\x07 ttB\x1b[201~"}, []string{"ttA", "ttB"}, []string{"\x1b", "\x07"}},
		{"fake_then_split_end", []string{"\x1b[200~fkA [201~ fkB\x1b[20", "1~"}, []string{"fkA", "fkB"}, []string{"\x1b[20"}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			a := start(t, 100, 24)
			a.waitFor("say something")
			a.settled()
			title0 := a.term.Snapshot().Title
			bracketedpasteedgeWrite(t, a, c.parts...)
			last := c.want[len(c.want)-1]
			a.waitUntil(func(s string) bool { return strings.Contains(bracketedpasteedgeDraft(s), last) }, "composer to hold "+last)
			s := a.settled()
			draft := bracketedpasteedgeDraft(s)
			pos := 0
			for _, w := range c.want {
				i := strings.Index(draft[pos:], w)
				if i < 0 {
					t.Fatalf("composer %q lacks %q in order:\n%s", draft, w, s)
				}
				pos += i + len(w)
			}
			for _, b := range c.bad {
				if strings.Contains(s, b) {
					t.Fatalf("screen shows %q:\n%s", b, s)
				}
			}
			if strings.Contains(s, "echo:") {
				t.Fatalf("paste was submitted before enter:\n%s", s)
			}
			snap := a.term.Snapshot()
			if snap.Title != title0 || strings.Contains(snap.Title, "pwned") {
				t.Fatalf("tab title changed by paste: %q -> %q", title0, snap.Title)
			}
			first, ok1 := pastewithescapesequencesinsideFg(snap, c.want[0])
			for _, w := range c.want[1:] {
				fg, ok := pastewithescapesequencesinsideFg(snap, w)
				if !ok1 || !ok {
					t.Fatalf("pasted words not found as cells:\n%s", s)
				}
				if fg != first {
					t.Fatalf("pasted %q coloured %v, %q is %v: escape leaked into styling", w, fg, c.want[0], first)
				}
			}
			// the paste has ended: a typed key lands after it
			a.typeText("Z")
			a.waitUntil(func(s string) bool { return strings.HasSuffix(bracketedpasteedgeDraft(s), last+"Z") }, "typed Z after paste")
			draft = bracketedpasteedgeDraft(a.settled())
			a.key(uv.KeyEnter, 0)
			if !a.waitDone(1, 30*time.Second) {
				t.Fatalf("turn never finished:\n%s", a.text())
			}
			got := strings.TrimSpace(bracketedpasteedgeInput(a))
			if strings.ContainsAny(got, "\x1b\x07") {
				t.Fatalf("sent prompt keeps raw escape bytes: %q", got)
			}
			if got != draft {
				t.Fatalf("sent %q, composer showed %q", got, draft)
			}
		})
	}
}
