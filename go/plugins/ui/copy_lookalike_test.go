package ui

import (
	"strings"
	"testing"
)

// Text that merely looks like a box (a pasted drawing, a table row) is
// not a box(): a drag over it copies every character, rails included.
func TestCopyDragKeepsBoxLookalikeText(t *testing.T) {
	for _, kind := range []string{"assistant", "user"} {
		d := defaultDrv(t)
		d.event(kind, "top\n╭──────╮\n│ a │ b │\n╰──────╯\nend")
		d.event("done", "")
		r0, r1 := frameRow(d, "top"), frameRow(d, "end")
		if r0 < 0 || r1 < 0 {
			t.Fatalf("%s: rows not found:\n%s", kind, d.plain())
		}
		text, _ := copyDrag(d, 0, r0, 79, r1)
		for _, want := range []string{"╭──────╮", "│ a │ b │", "╰──────╯"} {
			if !strings.Contains(text, want) {
				t.Fatalf("%s: %q lost from copy: %q", kind, want, text)
			}
		}
	}
}
