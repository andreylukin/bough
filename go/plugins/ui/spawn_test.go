package ui

import (
	"strings"
	"testing"
)

// Cards spawned together share an opening ("You are working in /tmp/x;
// the repo is…") which made three of them read identically. The shared
// part is dropped so the card shows what distinguishes it.
func TestSiblingCardsDropTheSharedOpening(t *testing.T) {
	m := testModel(t)
	pre := "You are working in /tmp/bough-live-goal; the repo is Go under go/. "
	for i, task := range []string{
		pre + "Set cache_control on the Anthropic system prompt.",
		pre + "Spill an oversized block result to a file.",
		pre + "Make the patch not-found error show the near match.",
	} {
		m.addEvent(Event{Kind: "sub:start", Text: task, Data: map[string]any{"worker": float64(i + 1)}})
	}
	frame := m.frame()
	for _, want := range []string{"Set cache_control", "Spill an oversized", "Make the patch"} {
		if !strings.Contains(frame, want) {
			t.Fatalf("card missing %q:\n%s", want, frame)
		}
	}
	if strings.Contains(frame, "You are working in") {
		t.Fatalf("the shared opening still fills the cards:\n%s", frame)
	}

	// A lone card keeps its task as written — there is nothing to
	// compare it against.
	one := testModel(t)
	one.addEvent(Event{Kind: "sub:start", Text: pre + "Do the thing.", Data: map[string]any{"worker": float64(1)}})
	if !strings.Contains(one.frame(), "You are working in") {
		t.Fatalf("a single card lost its opening:\n%s", one.frame())
	}
}

// line cuts by grapheme, never leaving a dangling ZWJ.
func TestLineKeepsGraphemesWhole(t *testing.T) {
	fam := "👨‍👩‍👧"
	if got, want := line("xx"+fam+"yy", 3), "xx"+fam+"…"; got != want {
		t.Fatalf("line = %q, want %q", got, want)
	}
	if got, want := line("xx"+fam+"yy", 2), "xx…"; got != want {
		t.Fatalf("line = %q, want %q", got, want)
	}
	if got := line("abc", 3); got != "abc" {
		t.Fatalf("line = %q, want %q", got, "abc")
	}
}
