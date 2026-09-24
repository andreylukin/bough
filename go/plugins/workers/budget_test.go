package workers

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/loop"
)

// The per-turn spawn budget counts the children that run or ran this
// turn: a child the provider killed never got to work and gives its slot
// back on every path, and a native child still running when its turn
// closed (adopted as a job) keeps its slot in the next one.
// specs/spawn_call_budget_cancel.fizz is the model of it.

func TestSpawnAllRefundsAProviderKilledChild(t *testing.T) {
	t.Parallel()
	l := subagentsTape(map[string][]string{
		"a":    {"Status: ok\nA"},
		"b":    {"ERR:provider 500"},
		"more": {"Status: ok\nMORE"},
	})
	r := subagentsMount(t, 5*time.Second, map[string]any{"max_spawns": 2}, l)
	out, err := r.cm.Run(`tools.spawnAll(["a","b"]).join("\n=====\n")`)
	if err != nil || !strings.Contains(out, "provider 500") {
		t.Fatalf("spawnAll = %q, %v", out, err)
	}
	out, err = r.cm.Run(`try { tools.spawn("more") } catch (e) { "REFUSED " + e }`)
	if err != nil || !strings.Contains(out, "MORE") {
		t.Fatalf("the killed child's slot was kept: spawn = %q, %v", out, err)
	}
}

// budgetEngine is the "engine" key's Spawn: "slow" runs until release
// closes, "down" is a child the provider killed, anything else finishes.
type budgetEngine struct {
	started chan string
	release chan struct{}
}

func (e *budgetEngine) Spawn(ctx context.Context, task, worker, system string, maxSteps int) (string, string, int, error) {
	e.started <- task
	switch task {
	case "slow":
		<-e.release
	case "down":
		return "subagent llm: provider down", "error", 1, nil
	}
	return "Status: ok\nFindings: " + task, "done", 1, nil
}

func budgetRig(t *testing.T, eng *budgetEngine) (*kernel.Context, agenttools.Tool) {
	t.Helper()
	home := t.TempDir()
	kctx := kernel.NewContext()
	reg := agenttools.NewRegistry()
	kctx.Provide("llm", &scriptLLM{script: []string{"unused"}})
	kctx.Provide("codemode", codemode.New(5*time.Second))
	kctx.Provide("history", &pathHist{path: filepath.Join(home, "sess-parent.jsonl")})
	kctx.Provide("agent-tools", reg)
	kctx.Provide("engine", eng)
	if err := apply(kctx, map[string]any{"max_spawns": 2}, home); err != nil {
		t.Fatal(err)
	}
	tl, ok := reg.Lookup("spawn")
	if !ok {
		t.Fatal("no native spawn")
	}
	return kctx, tl
}

func budgetSpawn(t *testing.T, tl agenttools.Tool, task string) agenttools.Result {
	t.Helper()
	args, _ := json.Marshal(map[string]string{"task": task})
	r, err := tl.Call(context.Background(), agenttools.Call{Args: args})
	if err != nil {
		t.Fatal(err)
	}
	return r
}

func TestNativeSpawnRefundsAProviderKilledChild(t *testing.T) {
	t.Parallel()
	eng := &budgetEngine{started: make(chan string, 8), release: make(chan struct{})}
	_, tl := budgetRig(t, eng)
	if r := budgetSpawn(t, tl, "down"); !strings.Contains(r.Error, "provider down") {
		t.Fatalf("killed child = %+v", r)
	}
	for i := range 2 {
		if r := budgetSpawn(t, tl, "fine"); r.Error != "" {
			t.Fatalf("spawn %d after a killed child = %+v: its slot was kept", i, r)
		}
	}
}

func TestDoneKeepsARunningNativeSpawnCounted(t *testing.T) {
	t.Parallel()
	eng := &budgetEngine{started: make(chan string, 8), release: make(chan struct{})}
	kctx, tl := budgetRig(t, eng)
	adopted := make(chan agenttools.Result, 1)
	go func() { adopted <- budgetSpawn(t, tl, "slow") }()
	<-eng.started
	// The turn closes with the child adopted as a job.
	kctx.Emit("loop/event", loop.Event{Kind: "done", Data: map[string]any{"running": 1}})
	if r := budgetSpawn(t, tl, "fine"); r.Error != "" {
		t.Fatalf("first spawn of the wake turn = %+v", r)
	}
	if r := budgetSpawn(t, tl, "fine"); !strings.Contains(r.Error, "spawn limit reached") {
		t.Fatalf("a spawn past the budget, with the adopted child still running, = %+v", r)
	}
	close(eng.release)
	if r := <-adopted; r.Error != "" {
		t.Fatalf("adopted child = %+v", r)
	}
}
