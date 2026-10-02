package vtreal

import (
	"fmt"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/charmbracelet/x/vt"
)

// History can already contain done while the terminal still shows the
// idle boot frame, followed by a late busy frame. Even a quiet partial
// reply must not release the content and no-execution assertions.
func TestMarkdownRenderedReplyWaitsForContentAndIdle(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		const cols, rows = 120, markdownRows
		term := &Terminal{Emu: vt.NewSafeEmulator(cols, rows), cols: cols, rows: rows}
		a := &app{t: t, term: term, cols: cols, rows: rows}
		w := stampWriter{term.Emu, &term.lastOut}
		paint := func(reply, status string) {
			fmt.Fprintf(w, "\x1b[2J\x1b[H❯ how do I print it?\r\n%s\x1b[%d;1H%s · ? keys\x1b[%d;1H> say something", reply, rows-1, status, rows)
		}
		want := []string{"hello from python", "ls -la /tmp/demo", "Either prints the same thing."}
		reply := strings.Join(want, "\r\n")
		paint("", "bough")
		done := make(chan struct{})
		defer func() { <-done; term.Emu.Close() }()
		go func() {
			defer close(done)
			time.Sleep(time.Second)
			paint("", "⠙ waiting for the model")
			for i := range want {
				time.Sleep(time.Second)
				partial := append([]string(nil), want[:i]...)
				partial = append(partial, want[i+1:]...)
				paint(strings.Join(partial, "\r\n"), "bough")
			}
			time.Sleep(time.Second)
			paint(reply, "⠙ streaming")
			time.Sleep(time.Second)
			paint(reply, "bough")
		}()
		began := time.Now()
		markdownWaitForRenderedReply(t, a, want...)
		if elapsed := time.Since(began); elapsed < 6*time.Second {
			t.Fatalf("accepted an unacknowledged reply after %s:\n%s", elapsed, a.text())
		}
		markdownWants(t, a, want...)
		if s := a.text(); strings.ContainsAny(s, spinnerFrames) {
			t.Fatalf("accepted a busy reply:\n%s", s)
		}
	})
}
