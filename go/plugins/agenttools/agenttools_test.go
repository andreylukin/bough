package agenttools

import (
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func TestProvidesAnEmptyRegistry(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		t.Fatalf("agent-tools not provided: %v", err)
	}
	if n := len(reg.Tools()); n != 0 {
		t.Fatalf("fresh registry holds %d tools", n)
	}
}
