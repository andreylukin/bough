package loop_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// hangLLM blocks until its context is cancelled: a provider mid-stream.
//
// The fallback is long on purpose. It exists only so a broken cancel
// cannot leave a goroutine parked forever — it must never RACE the
// test's own wait budget. At 10s it could, and did: on a loaded macOS
// runner the keypress landed after the fallback had already returned
// "too late" normally, so no cancelled row ever appeared and the test
// failed for the one reason it was not testing.
type hangLLM struct{}

func (hangLLM) Complete(ctx context.Context, _ string, _ []llm.Message) (string, error) {
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	case <-time.After(2 * time.Minute):
		return "too late", nil
	}
}

// ctrl+c during a turn reaches the loop's cancel service through the
// real model: the LLM call unwinds, the transcript shows a cancelled
// row (no error row), and the composer is free for the next prompt.
func TestCtrlCCancelsTurnEndToEnd(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", hangLLM{}) },
		"codemode", "loop")
	d.Say("take forever")
	d.WaitFor("❯ take forever")
	d.Press("ctrl+c")
	d.WaitFor("cancelled")
	if f := d.Frame(); strings.Contains(f, "✗") {
		t.Fatalf("cancel must not render as an error:\n%s", f)
	}
	// The turn's done lands right behind the cancelled row: the
	// spinner leaves the status bar.
	d.WaitUntil(func(f string) bool { return !strings.ContainsAny(f, "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏") }, "spinner to stop")
	// Idle again: one ctrl+c only arms the quit, the second quits.
	d.Press("ctrl+c")
	d.WaitFor("press ctrl+c again to quit")
	d.Press("ctrl+c")
	d.WaitQuit()
}
