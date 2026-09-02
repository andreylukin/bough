// TUI-integration tests: real kernel + llm rows + real ui model,
// driven in-process (see internal/uitest).
package llm_test

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// The llm-echo row's reply reaches the rendered transcript.
func TestEchoProviderRenders(t *testing.T) {
	t.Parallel()
	d := uitest.Mount(t, nil, "codemode", "llm-echo", "loop")
	d.Say("ping")
	d.WaitFor("echo: ping")
}

// A different provider (context-level stub) renders a different reply
// through the same loop -> event -> renderer path.
func TestStubProviderRenders(t *testing.T) {
	t.Parallel()
	stub := uitest.LLMFunc(func(system string, msgs []llm.Message) string {
		return "stub-reply-42 to: " + uitest.LastUser(msgs)
	})
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		"codemode", "loop")
	d.Say("ping")
	d.WaitFor("stub-reply-42 to: ping")
	if strings.Contains(d.Frame(), "echo: ping") {
		t.Fatalf("echo reply from nowhere:\n%s", d.Frame())
	}
}

// Last write wins on the "llm" key: an llm-echo row mounted after a
// context-level stub shadows it, and the transcript shows the row's
// provider, not the stub.
func TestRowProviderShadowsContextProvider(t *testing.T) {
	t.Parallel()
	stub := uitest.LLMFunc(func(string, []llm.Message) string { return "stub-should-lose" })
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		"codemode", "llm-echo", "loop")
	d.Say("ping")
	d.WaitFor("echo: ping")
	if strings.Contains(d.Frame(), "stub-should-lose") {
		t.Fatalf("shadowed provider answered:\n%s", d.Frame())
	}
}
