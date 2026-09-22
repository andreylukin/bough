package boughcall

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"strings"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/coordinator"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

// runOnce drives a real coordinator over a real localfile store with
// toolreg + boughcall + a real LocalOperationManager: one input, then
// stop when idle. It returns Run's error.
func runOnce(t *testing.T, dir string, sid session.ID, tools []agenttools.Tool, llm *fake.Adapter, inputID, text string, requests int) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	store, err := localfile.New(dir)
	if err != nil {
		t.Fatal(err)
	}
	restored, err := store.Resume(ctx, sid)
	if errors.Is(err, fs.ErrNotExist) {
		if _, err := store.Create(ctx, sid); err != nil {
			t.Fatal(err)
		}
		restored, err = store.Resume(ctx, sid)
	}
	if err != nil {
		t.Fatal(err)
	}
	in, err := inbox.New(ctx, restored.ExternalInputIDs)
	if err != nil {
		t.Fatal(err)
	}
	reg := agenttools.NewRegistry()
	for _, tl := range tools {
		reg.Register(tl)
	}
	tr := toolreg.New(toolreg.Config{Tools: reg.Tools()})
	b := contextbuilder.NewBuilder()
	for _, d := range tr.StaticDefinitions() {
		b.AddTool(d.Tool)
	}
	h := New(Options{Tools: reg.Lookup, Session: string(sid)})
	defer h.Close()
	ops := operation.NewLocalOperationManager(ctx, h)
	c := coordinator.New(coordinator.Dependencies{
		SessionID: sid, Inbox: in, Restored: restored, Sessions: store,
		ContextBuilder: b, LLM: llm, Tools: tr, Operations: ops,
	})
	done := make(chan error, 1)
	go func() { done <- c.Run(ctx) }()
	payload := jsontext.Value(`"` + text + `"`)
	if err := in.Submit(ctx, inbox.Input{ID: inbox.ID(inputID), Kind: inbox.InputExternal, Payload: payload}); err != nil {
		t.Fatal(err)
	}
	if err := llm.Wait(ctx, requests); err != nil {
		t.Fatal(err)
	}
	stop := jsontext.Value(`{"Mode":"when_idle"}`)
	if err := in.Submit(ctx, inbox.Input{ID: inbox.ID(inputID + "-stop"), Kind: inbox.InputControl, Payload: stop}); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		return err
	case <-ctx.Done():
		t.Fatal("coordinator did not stop")
		return nil
	}
}

// A store that recorded a call to a tool this build no longer has must
// still restore: the tombstone renders the recorded op, and the model
// reads the same result it read before.
func TestRetiredToolRestoresThroughTombstone(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	sid := session.ID("s-retired")
	retired := agenttools.Tool{Name: "retired", Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
		return agenttools.Result{Text: "old tool output"}, nil
	}}
	first := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Call("c1", "retired", `{}`)}},
		fake.Step{Output: []ullm.Item{fake.Text("done")}},
	)
	if err := runOnce(t, dir, sid, []agenttools.Tool{retired, echoTool()}, first, "in1", "use it", 2); err != nil {
		t.Fatalf("first run: %v", err)
	}
	second := fake.New(t, fake.Step{Output: []ullm.Item{fake.Text("again")}})
	if err := runOnce(t, dir, sid, []agenttools.Tool{echoTool()}, second, "in2", "once more", 1); err != nil {
		t.Fatalf("restore without the tool: %v", err)
	}
	req := second.Requests()[0].Request
	found := false
	for _, it := range req.Input {
		if r, ok := it.Data.(ullm.ToolResult); ok && r.CallID == "c1" {
			found = r.Output[0].Value == "old tool output"
		}
	}
	if !found {
		t.Fatalf("restored request lost the retired call's result:\n%s", fake.Render(req))
	}
	for _, tl := range req.Tools {
		if tl.Name == "retired" {
			t.Fatal("the retired tool is still offered")
		}
	}
	// A hallucinated name is answered, not fatal.
	third := fake.New(t,
		fake.Step{Output: []ullm.Item{fake.Call("c9", "retired", `{}`)}},
		fake.Step{Output: []ullm.Item{fake.Text("ok")}},
	)
	if err := runOnce(t, dir, sid, []agenttools.Tool{echoTool()}, third, "in3", "call it anyway", 2); err != nil {
		t.Fatalf("unknown tool call: %v", err)
	}
	last := third.Requests()[1].Request
	answered := false
	for _, it := range last.Input {
		if r, ok := it.Data.(ullm.ToolResult); ok && r.CallID == "c9" {
			answered = strings.Contains(r.Output[0].Value, `tool "retired" is not available in this session`)
		}
	}
	if !answered {
		t.Fatalf("unknown tool result missing:\n%s", fake.Render(last))
	}
}
