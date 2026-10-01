//go:build !windows

package engine

import (
	"context"
	"strings"
	"testing"
	"testing/synctest"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func TestLauncherStartupBarrierReachesRuntime(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ready := make(chan struct{})
		r := newRig(t, func(c *kernel.Context) {
			c.Provide("startup-ready", (<-chan struct{})(ready))
		})
		r.send("CODE!")
		synctest.Wait()
		if r.count("engine") != 0 {
			t.Fatal("coordinator started before the initial mount settled")
		}
		reg, err := kernel.Get[agenttools.Registry](r.ctx, "agent-tools")
		if err != nil {
			t.Fatal(err)
		}
		if _, err := reg.Register(agenttools.Tool{
			Name: "bash", Schema: agenttools.Object(nil, map[string]any{}),
			Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
				return agenttools.Result{Text: "startup tool works"}, nil
			},
		}); err != nil {
			t.Fatal(err)
		}
		close(ready)
		r.waitDone(1)
		if r.count("engine") != 1 || !strings.Contains(r.lastAssistant(), "startup tool works") {
			t.Fatalf("incomplete tool snapshot: %v", r.entries())
		}
	})
}
