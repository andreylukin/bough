package uitest_test

// Surface "provider-error-mid-tool-block": the provider's stream ends
// with an error after a ```js fence opened but before it closed. The
// half-written block must never run, the error shows once, the tokens
// the failed call spent still reach the status bar, and a resume of the
// same session file shows the same truncated turn and continues on a
// fresh provider.

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

const (
	providerErrorMidToolBlockProse = "Checking the tree first."
	providerErrorMidToolBlockErr   = "llm-openrouter: stream ended"
)

// providerErrorMidToolBlockLLM streams prose plus an unclosed js fence
// whose block would create marker, then fails. Usage reports what the
// failed call cost, as a provider that bills partial streams does.
type providerErrorMidToolBlockLLM struct {
	marker string
	mu     sync.Mutex
	failed bool
}

func (p *providerErrorMidToolBlockLLM) reply() string {
	return providerErrorMidToolBlockProse + "\n```js\n" + uitest.Bash("touch "+p.marker)
}

func (p *providerErrorMidToolBlockLLM) Complete(context.Context, string, []llm.Message) (string, error) {
	return "", errors.New("providerErrorMidToolBlockLLM streams only")
}

func (p *providerErrorMidToolBlockLLM) Stream(ctx context.Context, _ string, _ []llm.Message, onDelta func(string)) (string, error) {
	for _, d := range uitest.ByN(6)(p.reply()) {
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		onDelta(d)
	}
	p.mu.Lock()
	p.failed = true
	p.mu.Unlock()
	return "", errors.New(providerErrorMidToolBlockErr)
}

func (p *providerErrorMidToolBlockLLM) Usage() llm.Usage {
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.failed {
		return llm.Usage{}
	}
	return llm.Usage{InputTokens: 1234, OutputTokens: 56, LastInputTokens: 1234}
}

// providerErrorMidToolBlockMount mounts the real loop, codemode and
// tools-basic over stub, with the session log at path.
func providerErrorMidToolBlockMount(t *testing.T, stub any, path string) *uitest.Driver {
	t.Helper()
	open := history.Open // a missing file starts the session
	if _, err := os.Stat(path); err == nil {
		open = history.OpenExisting // an existing one resumes it, as {file: path} does
	}
	store, err := open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return uitest.Mount(t, func(c *kernel.Context) {
		c.Provide("llm", stub)
		c.Provide("history", store)
	}, "codemode", "tools-basic", "loop")
}

func providerErrorMidToolBlockKinds(t *testing.T, path string) []string {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	return kinds
}

func TestProviderErrorMidToolBlock(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	marker := filepath.Join(dir, "block-ran")
	path := filepath.Join(dir, "session.jsonl")
	stub := &providerErrorMidToolBlockLLM{marker: marker}

	d := providerErrorMidToolBlockMount(t, stub, path)
	d.Say("look around")
	turnDone(d, "stream ended")
	fits(t, d)
	live := d.Frame()

	t.Run("block not executed", func(t *testing.T) {
		if _, err := os.Stat(marker); err == nil {
			t.Fatalf("the unclosed js block ran:\n%s", live)
		}
		for _, k := range providerErrorMidToolBlockKinds(t, path) {
			if k == "code" || k == "result" || k == "assistant" {
				t.Fatalf("history recorded a %q entry for the failed call: %v", k, providerErrorMidToolBlockKinds(t, path))
			}
		}
		if strings.Contains(live, "▸ Ran") || strings.Contains(live, "▾ Ran") {
			t.Fatalf("a code block rendered as run:\n%s", live)
		}
	})

	t.Run("error visible once", func(t *testing.T) {
		if n := strings.Count(live, "stream ended"); n != 1 {
			t.Fatalf("error shown %d times:\n%s", n, live)
		}
		if !strings.Contains(live, "✗") {
			t.Fatalf("no error marker:\n%s", live)
		}
	})

	t.Run("cost chip counts partial tokens", func(t *testing.T) {
		if !strings.Contains(live, "↑1.2k ↓56") {
			t.Fatalf("status bar has no tally for the failed call:\n%s", live)
		}
	})

	// Known bug: the ui drops the live streaming block only on an
	// "assistant" event (model.go addEvent → dropLive); a call that
	// fails mid-stream never sends one, so the finished turn keeps a
	// blinking "▌" and "▸ writing code…" that a resume does not draw.
	t.Run("live partial reply settles on error", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_PROVIDER_ERROR_MID_TOOL_BLOCK") == "" {
			t.Skip("known bug: live assistant-delta block survives a provider error (ui addEvent never calls dropLive on error/done); set BOUGH_KNOWN_PROVIDER_ERROR_MID_TOOL_BLOCK=1 to run")
		}
		if strings.Contains(live, "▌") || strings.Contains(live, "writing code") {
			t.Fatalf("finished turn still shows the live streaming block:\n%s", live)
		}
	})

	t.Run("resume shows truncated state and continues", func(t *testing.T) {
		fresh := &uitest.Script{Replies: []string{"fresh tape answer"}}
		r := providerErrorMidToolBlockMount(t, fresh, path)
		r.WaitFor("resumed ")
		fits(t, r)
		resumed := r.Frame()
		if n := strings.Count(resumed, "stream ended"); n != 1 {
			t.Errorf("resumed frame shows the error %d times:\n%s", n, resumed)
		}
		if strings.Contains(resumed, providerErrorMidToolBlockProse) || strings.Contains(resumed, "writing code") {
			t.Errorf("resume drew the failed call's partial reply (history has none):\n%s", resumed)
		}
		if !strings.Contains(resumed, "look around") {
			t.Errorf("resumed frame lost the prompt:\n%s", resumed)
		}
		r.Say("carry on")
		turnDone(r, "fresh tape answer")
		fits(t, r)
		if _, err := os.Stat(marker); err == nil {
			t.Fatalf("the unclosed block ran after resume")
		}
		if fresh.Calls != 1 {
			t.Fatalf("fresh provider called %d times, want 1", fresh.Calls)
		}
	})
}
