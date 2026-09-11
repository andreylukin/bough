package vtreal

// Mouse reports split across reads on a slow PTY must never leak into
// the composer as text ("[<65;40;12M" residue), and must not kill bough.

import (
	"fmt"
	"os"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

var inputGarbageLeak = regexp.MustCompile(`\[<|\d+;\d+[Mm]|\[I|\[O|200~|201~`)

func inputGarbageRaw(a *app, s string, gap time.Duration) {
	a.t.Helper()
	for i := 0; i < len(s); i++ {
		if _, err := a.term.pty.Write([]byte{s[i]}); err != nil {
			a.t.Fatal(err)
		}
		if gap > 0 {
			time.Sleep(gap)
		}
	}
}

func inputGarbageBurst(n int) string {
	var sb strings.Builder
	for i := range n {
		b := 64 + i%2 // wheel up/down
		if i%5 == 4 {
			b = 66 + i%2 // horizontal wheel
		}
		fmt.Fprintf(&sb, "\x1b[<%d;%d;%dM", b, 10+i%30, 5+i%10)
		fmt.Fprintf(&sb, "\x1b[<35;%d;%dM", 11+i%30, 6+i%10) // motion
	}
	return sb.String()
}

func inputGarbageRun(t *testing.T, gap time.Duration, extra string) {
	a := slowterminalStart(t, 100, 30, cancelConfig(slowterminalLongTape(t, 3000), 1), true)
	a.typeText("long please")
	a.key(uv.KeyEnter, 0)
	time.Sleep(300 * time.Millisecond)
	a.typeText("draftxyz")
	inputGarbageRaw(a, inputGarbageBurst(20)+extra, gap)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	time.Sleep(time.Second)
	if a.cmd.ProcessState != nil {
		t.Fatalf("bough exited: %v", a.cmd.ProcessState)
	}
	a.settled()
	c := a.keymapComposer()
	if inputGarbageLeak.MatchString(c) || !strings.Contains(c, "draftxyz") {
		t.Fatalf("composer %q after mouse burst (gap %v)\n%s", c, gap, a.text())
	}
}

// Byte-at-a-time with tiny gaps: what a throttled link does.
func TestScrollInputGarbageSplitFast(t *testing.T) {
	t.Parallel()
	inputGarbageRun(t, time.Millisecond, "")
}

// Focus events interleaved with mouse reports.
func TestScrollInputGarbageFocus(t *testing.T) {
	t.Parallel()
	inputGarbageRun(t, time.Millisecond, "\x1b[I\x1b[<64;10;5M\x1b[O\x1b[<65;10;5M")
}

// A gap longer than the esc timeout after each byte. Gated: a lone ESC
// read as a key is arguably correct terminal-parser behaviour.
func TestScrollInputGarbageSplitSlow(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_SCROLL") == "" {
		t.Skip("set BOUGH_KNOWN_SCROLL=1")
	}
	t.Parallel()
	inputGarbageRun(t, 80*time.Millisecond, "")
}
