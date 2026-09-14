package ui

import "testing"

// The palette filters the "/" word the cursor is in, so a "/" typed
// in the middle of a prompt opens it, not only one at the end.
func TestSlashStartBeforeCursor(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		value     string
		line, col int
		want      int // slashStart of the text before the cursor
	}{
		{"fix /sk the bug", 0, 7, 4},      // mid-prompt, cursor after "/sk"
		{"fix the bug", 0, 4, -1},         // no slash word at the cursor
		{"/help", 0, 5, 0},                // line start still dispatches
		{"a\nlook /re later", 1, 8, 7},    // second line: offset counts "a\n"
		{"日本 /sk tail", 0, 6, len("日本 ")}, // rune columns, byte offsets
	} {
		v := tc.value
		if got := slashStart(v[:cursorByteOffset(v, tc.line, tc.col)]); got != tc.want {
			t.Errorf("%q at %d:%d: slashStart = %d, want %d", v, tc.line, tc.col, got, tc.want)
		}
	}
}
