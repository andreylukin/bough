package ui

import "testing"

// The mark is the first thing dropped: full size when the pane holds
// it, the small one when not, nothing in a short or narrow pane.
func TestMarkFits(t *testing.T) {
	t.Parallel()
	if a := mark(80, 40, 6); len(a) != len(markArt) {
		t.Fatalf("tall pane: want the full mark, got %d rows", len(a))
	}
	if a := mark(80, 24, 6); len(a) != len(markArtSmall) {
		t.Fatalf("24-row pane: want the small mark, got %d rows", len(a))
	}
	if a := mark(80, 12, 6); a != nil {
		t.Fatalf("short pane: want no mark, got %d rows", len(a))
	}
	if a := mark(8, 40, 6); a != nil {
		t.Fatalf("narrow pane: want no mark, got %d rows", len(a))
	}
}
