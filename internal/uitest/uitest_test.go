package uitest_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// Harness smoke: a real echo turn travels input chan -> loop -> llm ->
// loop/event -> model -> rendered frame.
func TestHarnessEchoTurn(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "loop")
	d.Say("hello harness")
	d.WaitFor("echo: hello harness")
	if !strings.Contains(d.Frame(), "❯ hello harness") {
		t.Fatalf("user line not rendered:\n%s", d.Frame())
	}
}

// Harness smoke: the default quit binding resolves through the real
// keymap service and surfaces as tea.Quit.
func TestHarnessDefaultQuitKey(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "loop")
	d.Press("ctrl+c")
	d.WaitQuit()
}
