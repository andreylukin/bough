package vtreal

// A skill invoked as /name with arguments that are not ASCII — emoji,
// CJK, an accented letter — plus a bracketed paste large enough to
// collapse to a placeholder. The echo llm replies with the exact last
// user message it was sent, so the assistant entry in history is the
// request log: the args, the expanded paste and the injected SKILL.md
// must all be in it byte for byte.

import (
	"fmt"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

const skillSlashWithUnicodeArgsAndPasteArgs = "héllo 🚀 日本語 ✓ "

// skillSlashWithUnicodeArgsAndPastePaste is 12 lines: over the
// composer's line threshold, so it collapses to a placeholder.
func skillSlashWithUnicodeArgsAndPastePaste() string {
	var b strings.Builder
	for i := range 12 {
		if i > 0 {
			b.WriteString("\n")
		}
		fmt.Fprintf(&b, "行%02d 🍣 paste", i)
	}
	return b.String()
}

func TestSkillSlashWithUnicodeArgsAndPasteRequestIsByteExact(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	paste := skillSlashWithUnicodeArgsAndPastePaste()
	a.typeText("/bravo " + skillSlashWithUnicodeArgsAndPasteArgs)
	a.waitFor("日本語")
	a.term.Paste(paste)
	a.waitFor(pastePlaceholder + "1 +12 lines]")
	a.typeText(" 尾🎉")
	a.waitFor("尾")
	a.key(uv.KeyEnter, 0)

	typed := "/bravo " + skillSlashWithUnicodeArgsAndPasteArgs + paste + " 尾🎉"
	want := "echo: " + typed + "\n\n[skill: bravo]\n---\ndescription: \"Bravo does the bravo job.\"\n---\nSKILLMARK_BRAVO body." // the stop-block unwrap trims the reply
	e := pasteWaitEntry(a, "SKILLMARK_BRAVO", "assistant")
	got, _ := e.Data["text"].(string)
	if !strings.Contains(got, want) {
		t.Fatalf("the llm request is not byte-exact\n got: %q\nwant: %q", got, want)
	}
	if strings.Contains(got, pastePlaceholder) {
		t.Fatalf("the paste placeholder reached the llm unexpanded: %q", got)
	}
	in := pasteWaitEntry(a, "SKILLMARK_BRAVO", "input")
	if ty, _ := in.Data["typed"].(string); ty != typed {
		t.Fatalf("input entry typed = %q, want %q", ty, typed)
	}
}

// Wide characters take two cells each: the virtual cursor must sit
// right after the last one, not at the rune count or the byte count.
func TestSkillSlashWithUnicodeArgsAndPasteCursorCell(t *testing.T) {
	t.Parallel()
	a := skillsStart(t)
	a.typeText("/bravo é🚀日本")
	a.waitFor("> /bravo é🚀日本")
	a.settled()
	snap := a.term.Snapshot()
	row := composerRow(a.lines())
	// "> /bravo " 9 + é 1 + 🚀 2 + 日本 4 = 16
	const col = 16
	cell := snap.Cells[row][col]
	if cell.Style.Attrs&uv.AttrReverse == 0 {
		var attrs []string
		for x := 0; x < 24 && x < len(snap.Cells[row]); x++ {
			attrs = append(attrs, fmt.Sprintf("%d:%q:%b", x, snap.Cells[row][x].Content, snap.Cells[row][x].Style.Attrs&uv.AttrReverse))
		}
		t.Fatalf("no reverse-video cursor at col %d; row cells %v\n%s", col, attrs, a.text())
	}
	for x := 0; x < col; x++ {
		if snap.Cells[row][x].Style.Attrs&uv.AttrReverse != 0 {
			t.Fatalf("reverse-video cell at col %d before the draft end (%d):\n%s", x, col, a.text())
		}
	}
}
