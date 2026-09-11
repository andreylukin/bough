package vtreal

// A bracketed paste dribbled into the PTY a few bytes at a time (a slow
// ssh link) must still arrive as one paste: one placeholder, and the
// loop gets the text byte for byte.

import (
	"fmt"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func TestPasteSlowPTYManyReads(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	var b strings.Builder
	for i := range 40 {
		fmt.Fprintf(&b, "slowline%02d ünï 🐛\n", i)
	}
	text := strings.TrimSuffix(b.String(), "\n")
	raw := "\x1b[200~" + text + "\x1b[201~"
	for len(raw) > 0 {
		n := min(7, len(raw))
		if _, err := a.term.pty.Write([]byte(raw[:n])); err != nil {
			t.Fatal(err)
		}
		raw = raw[n:]
		time.Sleep(2 * time.Millisecond)
	}
	a.waitFor(pastePlaceholder + "1 +40 lines]")
	if s := a.settled(); strings.Contains(s, pastePlaceholder+"2") || strings.Contains(s, "slowline05") {
		t.Fatalf("the slow paste split into pieces:\n%s", s)
	}
	a.key(uv.KeyEnter, 0)
	e := pasteWaitEntry(a, "slowline39", "input")
	if got, _ := e.Data["text"].(string); got != text {
		t.Fatalf("loop got %d bytes, want %d:\n%q", len(got), len(text), got)
	}
}
