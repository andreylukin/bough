package workers

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

// fakeEngine is the "engine" key's Spawn, scripted per task.
type fakeEngine struct {
	mu    sync.Mutex
	calls []string // the worker names asked for, in order
	reply map[string][3]string
}

func (e *fakeEngine) Spawn(_ context.Context, task, worker, system string, maxSteps int) (string, string, int, error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.calls = append(e.calls, worker)
	if system != nativeSubSystemPrompt || maxSteps != defaultMaxSteps {
		return "", "error", 0, errors.New("wrong system or budget")
	}
	r, ok := e.reply[task]
	if !ok {
		return "", "error", 0, errors.New("provider down")
	}
	return r[0], r[1], 3, nil
}

func nativeRig(t *testing.T, eng *fakeEngine) (agenttools.Registry, *bgRig) {
	t.Helper()
	reg := agenttools.NewRegistry()
	r := bgMount(t, true, func(k *kernel.Context) {
		k.Provide("agent-tools", reg)
		if eng != nil {
			k.Provide("engine", eng)
		}
	}, nil)
	return reg, r
}

func nativeCall(t *testing.T, reg agenttools.Registry, name, worker, args string) agenttools.Result {
	t.Helper()
	tl, ok := reg.Lookup(name)
	if !ok {
		t.Fatalf("no native %s", name)
	}
	r, err := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(args), Worker: worker})
	if err != nil {
		t.Fatal(err)
	}
	return r
}

// A foreground native spawn is the engine's child run, numbered and
// labelled like tools.spawn's; a budget stop is a partial report, any
// other end fails the call; a subagent cannot spawn.
func TestNativeSpawnRunsTheEngineChild(t *testing.T) {
	t.Parallel()
	eng := &fakeEngine{reply: map[string][3]string{
		"survey plugins": {"Status: ok\nFindings: 35 plugins", "done"},
		"read it all":    {"Status: failed\nFindings: ran out", "budget"},
		"give up":        {"", "cancelled"},
	}}
	reg, _ := nativeRig(t, eng)
	if got := names(reg); got != "agent,spawn,stop_agent" {
		t.Fatalf("tools = %s", got)
	}
	r := nativeCall(t, reg, "spawn", "", `{"task":"survey plugins"}`)
	if r.Error != "" || r.Text != "[subagent 1 · task: survey plugins]\nStatus: ok\nFindings: 35 plugins" || r.Data["status"] != "done" || r.Data["worker"] != 1 {
		t.Fatalf("spawn = %+v", r)
	}
	if r := nativeCall(t, reg, "spawn", "", `{"task":"read it all"}`); r.Error != "" || !strings.Contains(r.Text, "step budget") {
		t.Fatalf("budget = %+v", r)
	}
	if r := nativeCall(t, reg, "spawn", "", `{"task":"give up"}`); r.Error != "workers: subagent 3 ended cancelled" {
		t.Fatalf("cancelled = %+v", r)
	}
	if r := nativeCall(t, reg, "spawn", "", `{"task":"unknown"}`); !strings.Contains(r.Error, "provider down") {
		t.Fatalf("error = %+v", r)
	}
	if r := nativeCall(t, reg, "spawn", "subagent 1", `{"task":"nested"}`); r.Error != "workers: subagent depth 1 only" {
		t.Fatalf("nested = %+v", r)
	}
	if r := nativeCall(t, reg, "spawn", "", `{"task":"x","project":"demo"}`); !strings.Contains(r.Error, "background: true") {
		t.Fatalf("foreground with project = %+v", r)
	}
	eng.mu.Lock()
	defer eng.mu.Unlock()
	if strings.Join(eng.calls, ",") != "subagent 1,subagent 2,subagent 3,subagent 4" {
		t.Fatalf("workers = %v", eng.calls)
	}
}

// The per-turn spawn budget holds natively: parallel spawn calls are
// what spawnAll was, and they count one each.
func TestNativeSpawnBudget(t *testing.T) {
	t.Parallel()
	eng := &fakeEngine{reply: map[string][3]string{"t": {"ok", "done"}}}
	reg, _ := nativeRig(t, eng)
	for i := 0; i < defaultMaxSpawns; i++ {
		if r := nativeCall(t, reg, "spawn", "", `{"task":"t"}`); r.Error != "" {
			t.Fatalf("spawn %d = %+v", i, r)
		}
	}
	if r := nativeCall(t, reg, "spawn", "", `{"task":"t"}`); !strings.Contains(r.Error, "spawn limit reached") {
		t.Fatalf("over budget = %+v", r)
	}
}

func TestNativeSpawnWithoutEngine(t *testing.T) {
	t.Parallel()
	reg, _ := nativeRig(t, nil)
	if r := nativeCall(t, reg, "spawn", "", `{"task":"t"}`); r.Error != "workers: a native spawn needs the engine-unreal row" {
		t.Fatalf("no engine = %+v", r)
	}
}

// background: true, agent and stop_agent are the serve calls tools.spawn
// makes, answered as JSON text.
func TestNativeBackgroundAgent(t *testing.T) {
	t.Parallel()
	reg, r := nativeRig(t, nil)
	r.serve.codes["/api/sessions"] = 201
	r.serve.reply["/api/sessions"] = `{"session":{"id":"child-1"},"queued":false}`
	res := nativeCall(t, reg, "spawn", "", `{"task":"list go files","background":true,"project":"demo"}`)
	if res.Error != "" || !sameJSON(res.Text, `{"session":"child-1","status":"running"}`) || res.Data["session"] != "child-1" {
		t.Fatalf("background spawn = %+v", res)
	}
	var req map[string]any
	json.Unmarshal([]byte(r.serve.body("/api/sessions")), &req)
	if req["spawnedBy"] != "sess-parent" || req["prompt"] != "list go files" || req["slug"] != "demo" {
		t.Fatalf("request = %v", req)
	}
	r.serve.reply["/api/sessions/child-1/agent"] = `{"status":"done","title":"Go files","reply":"12 files"}`
	if res := nativeCall(t, reg, "agent", "", `{"id":"child-1"}`); res.Error != "" || !strings.Contains(res.Text, `"reply":"12 files"`) {
		t.Fatalf("agent = %+v", res)
	}
	r.serve.codes["/api/sessions/nope/agent"] = 404
	if res := nativeCall(t, reg, "agent", "", `{"id":"nope"}`); res.Error != `workers: no agent "nope"` {
		t.Fatalf("unknown agent = %+v", res)
	}
}

func names(reg agenttools.Registry) string {
	var n []string
	for _, tl := range reg.Tools() {
		n = append(n, tl.Name)
	}
	return strings.Join(n, ",")
}
