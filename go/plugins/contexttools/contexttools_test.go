package contexttools

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/contextkit"
	"github.com/andreylukin/bough/kernel"
)

type fakeCapability struct {
	name string
	read func(contextkit.ReadRequest) (contextkit.ReadResult, error)
	edit func(contextkit.EditRequest) (contextkit.EditResult, error)
}

func (f *fakeCapability) Inspect(context.Context) (contextkit.Info, error) {
	return contextkit.Info{SnapshotID: f.name, Revision: f.name + "-revision", Bytes: len(f.name)}, nil
}
func (f *fakeCapability) Read(_ context.Context, a contextkit.ReadRequest) (contextkit.ReadResult, error) {
	if f.read != nil {
		return f.read(a)
	}
	return contextkit.ReadResult{Info: contextkit.Info{SnapshotID: f.name}, Text: f.name, Offset: a.Offset}, nil
}
func (f *fakeCapability) Search(context.Context, contextkit.SearchRequest) (contextkit.SearchResult, error) {
	return contextkit.SearchResult{Info: contextkit.Info{SnapshotID: f.name}}, nil
}
func (f *fakeCapability) Edit(_ context.Context, a contextkit.EditRequest) (contextkit.EditResult, error) {
	if f.edit != nil {
		return f.edit(a)
	}
	return contextkit.EditResult{Revision: a.ExpectedRevision, DryRun: a.DryRun}, nil
}
func (f *fakeCapability) Offload(_ context.Context, a contextkit.OffloadRequest) (contextkit.OffloadResult, error) {
	return contextkit.OffloadResult{EditResult: contextkit.EditResult{Revision: a.ExpectedRevision, DryRun: a.DryRun}, ArchiveID: f.name}, nil
}
func (f *fakeCapability) Restore(_ context.Context, a contextkit.RestoreRequest) (contextkit.EditResult, error) {
	return contextkit.EditResult{Revision: a.ExpectedRevision, DryRun: a.DryRun}, nil
}

func toolNamed(t *testing.T, name string) agenttools.Tool {
	t.Helper()
	for _, tool := range tools() {
		if tool.Name == name {
			return tool
		}
	}
	t.Fatalf("missing tool %s", name)
	return agenttools.Tool{}
}

func TestRegistersScopedToolsAndUnmounts(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	reg := agenttools.NewRegistry()
	ctx.Provide("agent-tools", reg)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	if len(reg.Tools()) != 6 {
		t.Fatalf("got %d tools", len(reg.Tools()))
	}
	for _, tool := range reg.Tools() {
		if !tool.RequiresContext || tool.Blocking || tool.Schema["additionalProperties"] != false {
			t.Errorf("invalid capability metadata for %s", tool.Name)
		}
		wantMutation := tool.Name == "context_edit" || tool.Name == "context_offload" || tool.Name == "context_restore"
		if tool.MutatesContext != wantMutation {
			t.Errorf("mutation metadata for %s = %v", tool.Name, tool.MutatesContext)
		}
	}
	ctx.Unmount()
	if len(reg.Tools()) != 0 {
		t.Fatal("unmount left context tools registered")
	}
}

func TestRegistrationFailureRollsBack(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	defer ctx.Unmount()
	reg := agenttools.NewRegistry()
	ctx.Provide("agent-tools", reg)
	if _, err := reg.Register(toolNamed(t, "context_read")); err != nil {
		t.Fatal(err)
	}
	if err := (plugin{}).Apply(ctx, nil); err == nil {
		t.Fatal("duplicate registration succeeded")
	}
	if got := reg.Tools(); len(got) != 1 || got[0].Name != "context_read" {
		t.Fatalf("failed mount left partial registrations: %v", got)
	}
}

func TestCallUsesItsOwnCapability(t *testing.T) {
	t.Parallel()
	tool := toolNamed(t, "context_inspect")
	for _, name := range []string{"parent", "child-one", "child-two"} {
		res, err := tool.Call(context.Background(), agenttools.Call{Context: &fakeCapability{name: name}})
		if err != nil || res.Error != "" {
			t.Fatalf("%s: %v %v", name, err, res)
		}
		var info contextkit.Info
		if err := json.Unmarshal([]byte(res.Text), &info); err != nil || info.SnapshotID != name {
			t.Fatalf("capability leaked across calls: %s, %v", res.Text, err)
		}
	}
	res, _ := tool.Call(context.Background(), agenttools.Call{})
	if res.Error == "" {
		t.Fatal("missing capability was accepted")
	}
}

func TestArgumentsAndCancellationFailBeforeInvocation(t *testing.T) {
	t.Parallel()
	tool := toolNamed(t, "context_edit")
	cap := &fakeCapability{edit: func(contextkit.EditRequest) (contextkit.EditResult, error) {
		t.Error("invalid call invoked the context")
		return contextkit.EditResult{}, nil
	}}
	for _, args := range []string{`{"dryrun":true}`, `{} {}`, `null`, `[]`, `{}`,
		`{"expected_revision":"r","edits":[{"start":0,"end":1}]}`,
		`{"expected_revision":"r","edits":[{"end":1,"text":""}]}`,
		`{"expected_revision":"r","edits":[null]}`,
		`{"expected_revision":"r","edits":[] ,"dry_run":null}`,
		`{"expected_revision":"r","edits":[{"start":0,"end":1,"text":null}]}`,
		"{\"expected_revision\":\"r\",\"edits\":[{\"start\":0,\"end\":0,\"text\":\"\xff\"}]}",
	} {
		res, _ := tool.Call(context.Background(), agenttools.Call{Args: json.RawMessage(args), Context: cap})
		if res.Error == "" {
			t.Errorf("invalid arguments accepted: %s", args)
		}
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	res, _ := tool.Call(ctx, agenttools.Call{Args: json.RawMessage(`{}`), Context: cap})
	if res.Error == "" || res.Data["error_code"] != "cancelled" {
		t.Fatalf("cancellation not preserved: %v", res)
	}
}

func TestStructuredConflictAndDryRun(t *testing.T) {
	t.Parallel()
	tool := toolNamed(t, "context_edit")
	cap := &fakeCapability{edit: func(a contextkit.EditRequest) (contextkit.EditResult, error) {
		if !a.DryRun || a.ExpectedRevision != "old" || len(a.Edits) != 1 || a.Edits[0].Text != "new" {
			t.Fatalf("request was changed: %+v", a)
		}
		return contextkit.EditResult{}, &contextkit.Conflict{Expected: "old", Current: "new"}
	}}
	res, err := tool.Call(context.Background(), agenttools.Call{Context: cap, Args: json.RawMessage(`{"expected_revision":"old","edits":[{"start":0,"end":0,"text":"new"}],"dry_run":true}`)})
	if err != nil || res.Error == "" || res.Data["error_code"] != "revision_conflict" {
		t.Fatalf("conflict lost: %v %v", res, err)
	}
	var data map[string]any
	if err := json.Unmarshal([]byte(res.Text), &data); err != nil || data["expected_revision"] != "old" || data["current_revision"] != "new" {
		t.Fatalf("conflict revisions missing: %s %v", res.Text, err)
	}
	if got := failure("context_read", fmtWrapped(contextkit.ErrSnapshotExpired)); got.Data["error_code"] != "snapshot_expired" {
		t.Fatal("wrapped snapshot expiry lost")
	}
}

func fmtWrapped(err error) error { return errors.Join(errors.New("lookup"), err) }

func TestReadCallsOverlap(t *testing.T) {
	t.Parallel()
	entered := make(chan struct{}, 2)
	release := make(chan struct{})
	defer close(release)
	cap := &fakeCapability{read: func(a contextkit.ReadRequest) (contextkit.ReadResult, error) {
		entered <- struct{}{}
		<-release
		return contextkit.ReadResult{Info: contextkit.Info{SnapshotID: a.SnapshotID}}, nil
	}}
	tool := toolNamed(t, "context_read")
	var wg sync.WaitGroup
	for range 2 {
		wg.Go(func() {
			_, _ = tool.Call(context.Background(), agenttools.Call{Context: cap, Args: json.RawMessage(`{"snapshot_id":"shared"}`)})
		})
	}
	for range 2 {
		select {
		case <-entered:
		case <-time.After(3 * time.Second):
			t.Fatal("a read was serialized behind another read")
		}
	}
	// Both entered before either was released; completion is checked by
	// cleanup after the release, without an elapsed-time performance claim.
	t.Cleanup(wg.Wait)
}

func TestDecoderErrorIsBounded(t *testing.T) {
	t.Parallel()
	args, err := json.Marshal(map[string]any{strings.Repeat("<", 20000): true})
	if err != nil {
		t.Fatal(err)
	}
	res, err := toolNamed(t, "context_inspect").Call(context.Background(), agenttools.Call{Args: args, Context: &fakeCapability{name: "test"}})
	if err != nil || res.Error == "" || len(res.Text)+len(res.Error) > 10000 {
		t.Fatalf("unbounded or missing argument error: text=%d error=%d returned=%v", len(res.Text), len(res.Error), err)
	}
	if !strings.Contains(res.Error, "truncated") {
		t.Fatal("truncated diagnostic was not identified")
	}
}
