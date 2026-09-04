package ui

import (
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// A configured llm-small row that provides no llm-small service is the
// silent failure: without `service: llm-small` the row publishes under
// "llm" and replaces the agent's model instead. Say so where a person
// looks, not in a log.
func TestSmallRowConfiguredDetectsBothShapes(t *testing.T) {
	cases := []struct {
		name string
		rows []kernel.Row
		want bool
	}{
		{"by id", []kernel.Row{{ID: "llm-small", Plugin: "llm-openrouter"}}, true},
		{"by service key", []kernel.Row{{ID: "cheap", Plugin: "llm-openrouter",
			Config: map[string]any{"service": "llm-small"}}}, true},
		{"only the main row", []kernel.Row{{ID: "llm", Plugin: "llm-openrouter",
			Config: map[string]any{"model": "x"}}}, false},
		{"nothing", nil, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			ctx := kernel.NewContext()
			if err := ctx.Mount(c.rows); err != nil && len(c.rows) > 0 {
				// The rows need not mount (no plugin registered here);
				// Mount still records them as desired, which is what
				// the check reads.
				_ = err
			}
			if got := smallRowConfigured(ctx); got != c.want {
				t.Fatalf("smallRowConfigured = %v, want %v", got, c.want)
			}
		})
	}
}
