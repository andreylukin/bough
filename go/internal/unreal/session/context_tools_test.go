//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/operation"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/contextkit"
	"github.com/andreylukin/bough/internal/unreal/boughcall"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

func contextToolCall(t *testing.T, h *boughcall.Handler, id, name, args string) {
	t.Helper()
	b, err := json.Marshal(toolreg.Plan{Call: id, Tool: name, Args: json.RawMessage(args)})
	if err != nil {
		t.Fatal(err)
	}
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: toolreg.PlanType, Version: toolreg.PlanVersion, Data: jsontext.Value(b)})
	if err != nil {
		t.Fatal(err)
	}
	if err := h.AddRemoteJob(operation.Operation{ID: operation.ID(id), Type: spec.Type, Version: spec.Version, MaxOutputLength: spec.MaxOutputLength, Status: operation.StatusReady, State: spec.State}); err != nil {
		t.Fatal(err)
	}
}

func contextReceive[T any](t *testing.T, ch <-chan T) T {
	t.Helper()
	select {
	case v := <-ch:
		return v
	case <-time.After(5 * time.Second):
		t.Fatal("context tool did not reach the barrier")
		var zero T
		return zero
	}
}

// Waiting for both callbacks BEFORE releasing either proves overlap;
// merely seeing two successful calls does not detect a global mutex.
func TestCLMUnrelatedNativeWritesOverlap(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode})
	entered := make(chan string, 2)
	release := make(chan struct{})
	defer close(release)
	_, err := r.kit.reg.Register(agenttools.Tool{Name: "write", Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
		entered <- c.ID
		select {
		case <-release:
		case <-ctx.Done():
		}
		return agenttools.Result{}, ctx.Err()
	}})
	if err != nil {
		t.Fatal(err)
	}
	h := r.rt.newHandler(r.rt.gate, "", nil)
	defer h.Close()
	contextToolCall(t, h, "a", "write", `{}`)
	contextToolCall(t, h, "b", "write", `{}`)
	if a, b := contextReceive(t, entered), contextReceive(t, entered); a == b {
		t.Fatalf("same call entered twice: %q", a)
	}
}

// The owning context coordinates projection and file tools, while a
// sibling's projection remains independent. A canceled queued writer
// must never invoke its mutation callback.
func TestCLMManagedMutationIsScopedAndCancelable(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode})
	main, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	child := newGate(r.rt, "same", nil)
	child.contextID = "distinct-child"
	sibling, err := child.context()
	if err != nil {
		t.Fatal(err)
	}
	path, _ := agenttools.CanonicalFile(main.Path())
	entered := make(chan struct{})
	release := make(chan struct{})
	var releaseOnce sync.Once
	defer releaseOnce.Do(func() { close(release) })
	first := make(chan error, 1)
	go func() {
		first <- r.rt.mutateContextFile(t.Context(), path, func() error {
			close(entered)
			<-release
			return os.WriteFile(path, []byte("retained"), 0600)
		})
	}()
	contextReceive(t, entered)
	ctx, cancel := context.WithCancel(t.Context())
	queued := make(chan error, 1)
	attempted := make(chan struct{})
	go func() {
		close(attempted)
		queued <- r.rt.mutateContextFile(ctx, path, func() error {
			t.Error("canceled mutation ran")
			return nil
		})
	}()
	contextReceive(t, attempted)
	cancel()
	if err := contextReceive(t, queued); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled mutation: %v", err)
	}
	other := make(chan error, 1)
	go func() { other <- sibling.Append([]ullmItem{fake.Text("sibling output")}) }()
	if err := contextReceive(t, other); err != nil {
		t.Fatal(err)
	}
	appendDone := make(chan error, 1)
	go func() { appendDone <- main.Append([]ullmItem{fake.Text("main output")}) }()
	releaseOnce.Do(func() { close(release) })
	if err := contextReceive(t, first); err != nil {
		t.Fatal(err)
	}
	if err := contextReceive(t, appendDone); err != nil {
		t.Fatal(err)
	}
	b, _ := os.ReadFile(path)
	if !strings.Contains(string(b), "retained") || !strings.Contains(string(b), "main output") {
		t.Fatalf("projection and managed write lost each other: %s", b)
	}
}

// Binding a capability must not call Context.Path while a commit holds
// its gate: reads of an existing immutable snapshot need no live-file lock.
func TestCLMNativeSnapshotReadsOverlapManagedMutation(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode})
	c, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	path := c.Path()
	if err := os.WriteFile(path, []byte("retained snapshot"), 0600); err != nil {
		t.Fatal(err)
	}
	info, err := c.Inspect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	answers := make(chan error, 2)
	for _, name := range []string{"context_read", "context_search"} {
		_, err := r.kit.reg.Register(agenttools.Tool{Name: name, RequiresContext: true, Call: func(ctx context.Context, call agenttools.Call) (agenttools.Result, error) {
			var err error
			if name == "context_read" {
				var read contextkit.ReadResult
				read, err = call.Context.Read(ctx, contextkit.ReadRequest{SnapshotID: info.SnapshotID})
				if err == nil && read.Text != "retained snapshot" {
					err = errors.New("snapshot read changed")
				}
			} else {
				var found contextkit.SearchResult
				found, err = call.Context.Search(ctx, contextkit.SearchRequest{SnapshotID: info.SnapshotID, Query: "snapshot"})
				if err == nil && len(found.Matches) != 1 {
					err = errors.New("snapshot search changed")
				}
			}
			answers <- err
			return agenttools.Result{}, err
		}})
		if err != nil {
			t.Fatal(err)
		}
	}
	h := r.rt.newHandler(r.rt.gate, "", nil)
	defer h.Close()
	held := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	defer once.Do(func() { close(release) })
	written := make(chan error, 1)
	go func() {
		_, err := c.MutateFile(t.Context(), path, func() error { close(held); <-release; return nil })
		written <- err
	}()
	contextReceive(t, held)
	contextToolCall(t, h, "read", "context_read", `{}`)
	contextToolCall(t, h, "search", "context_search", `{}`)
	for range 2 {
		if err := contextReceive(t, answers); err != nil {
			t.Fatal(err)
		}
	}
	once.Do(func() { close(release) })
	if err := contextReceive(t, written); err != nil {
		t.Fatal(err)
	}
}

func TestCLMContextCapabilityChecksLiveWritePolicy(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode})
	c, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	cap := authorizedContext{Capability: c, runtime: r.rt, path: c.Path()}
	initial, err := cap.Inspect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	// Seed a real archive before applying policy. Restore must be an
	// otherwise-valid mutation, not a rejection of a made-up reference.
	seed, err := c.Edit(t.Context(), contextkit.EditRequest{ExpectedRevision: initial.Revision, Edits: []contextkit.Edit{{End: initial.Bytes, Text: "archive\nretained notes\n"}}})
	if err != nil {
		t.Fatal(err)
	}
	empty := ""
	archive, err := c.Offload(t.Context(), contextkit.OffloadRequest{ExpectedRevision: seed.Revision, End: len("archive\n"), Replacement: &empty})
	if err != nil {
		t.Fatal(err)
	}
	info, err := cap.Inspect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	mutations := []struct {
		name string
		call func(bool) error
	}{
		{"edit", func(dry bool) error {
			_, err := cap.Edit(t.Context(), contextkit.EditRequest{ExpectedRevision: info.Revision, Edits: []contextkit.Edit{{Text: "not allowed"}}, DryRun: dry})
			return err
		}},
		{"offload", func(dry bool) error {
			_, err := cap.Offload(t.Context(), contextkit.OffloadRequest{ExpectedRevision: info.Revision, End: info.Bytes, DryRun: dry})
			return err
		}},
		{"restore", func(dry bool) error {
			_, err := cap.Restore(t.Context(), contextkit.RestoreRequest{ExpectedRevision: info.Revision, ArchiveID: archive.ArchiveID, Offset: info.Bytes, End: archive.ArchivedBytes, DryRun: dry})
			return err
		}},
	}
	readDisk := func(t *testing.T) map[string]string {
		t.Helper()
		files := make(map[string]string)
		err := filepath.WalkDir(r.dir, func(path string, entry os.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if entry.IsDir() {
				files[path+string(filepath.Separator)] = ""
				return nil
			}
			b, err := os.ReadFile(path)
			files[path] = string(b)
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
		return files
	}
	before := readDisk(t)
	checkDenied := func(stage string, want error) {
		t.Helper()
		for _, mutation := range mutations {
			t.Run(stage+"/"+mutation.name, func(t *testing.T) {
				err := mutation.call(false)
				if want != nil {
					if !errors.Is(err, want) {
						t.Fatalf("path refusal lost: %v", err)
					}
				} else if err == nil || !strings.Contains(err.Error(), "require the session's native write tool and its path policy") {
					t.Fatalf("mutation without native write authority: %v", err)
				}
				if after := readDisk(t); !reflect.DeepEqual(before, after) {
					t.Fatal("refused mutation changed live notes, private cursor, or archives")
				}
				if err := mutation.call(true); err != nil {
					t.Fatalf("valid dry-run required mutation permission: %v", err)
				}
				if after := readDisk(t); !reflect.DeepEqual(before, after) {
					t.Fatal("dry-run changed live notes, private cursor, or archives")
				}
			})
		}
	}
	checkDenied("absent", nil)
	write := agenttools.Tool{Name: "write", Call: func(context.Context, agenttools.Call) (agenttools.Result, error) { return agenttools.Result{}, nil }}
	unregister, err := r.kit.reg.Register(write)
	if err != nil {
		t.Fatal(err)
	}
	checkDenied("missing-authority", nil)
	unregister()
	denied := errors.New("configured path refusal")
	checks := 0
	write.WriteAllowed = func(_ context.Context, path string) error {
		checks++
		if path != c.Path() {
			t.Errorf("checked %q, want current context %q", path, c.Path())
		}
		return denied
	}
	unregister, err = r.kit.reg.Register(write)
	if err != nil {
		t.Fatal(err)
	}
	checkDenied("denied", denied)
	if checks != len(mutations) {
		t.Fatalf("policy checked %d times, want each mutation once and no dry-runs", checks)
	}
	unregister()
	write.WriteAllowed = func(context.Context, string) error { return nil }
	if _, err := r.kit.reg.Register(write); err != nil {
		t.Fatal(err)
	}
	if err := mutations[0].call(false); err != nil {
		t.Fatal(err)
	}
}

func TestContextToolsAbsentOutsideCLM(t *testing.T) {
	t.Parallel()
	r := newRig(t)
	called := make(chan struct{}, 1)
	_, err := r.kit.reg.Register(agenttools.Tool{Name: "context_inspect", RequiresContext: true, Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
		called <- struct{}{}
		return agenttools.Result{}, nil
	}})
	if err != nil {
		t.Fatal(err)
	}
	for _, tool := range r.rt.snapshot() {
		if tool.Name == "context_inspect" {
			t.Fatal("unreal advertises a CLM-only tool")
		}
	}
	h := r.rt.newHandler(r.rt.gate, "", nil)
	defer h.Close()
	contextToolCall(t, h, "unknown", "context_inspect", `{}`)
	for {
		op := contextReceive(t, h.RemoteJobUpdates())
		if op.Status == operation.StatusFailed {
			break
		}
	}
	select {
	case <-called:
		t.Fatal("unreal dispatched a CLM-only tool")
	default:
	}
}

// A live registry can forward Changed asynchronously. Authorization
// must also check the registration synchronously at commit time.
type quietRegistry struct {
	agenttools.Registry
	quiet chan struct{}
}

func (r quietRegistry) Changed() <-chan struct{} { return r.quiet }

func TestCLMQueuedMutationRejectsReplacedWritePolicy(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode, func(d *Deps) {
		d.Tools = quietRegistry{Registry: d.Tools, quiet: make(chan struct{})}
	}})
	c, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	info, err := c.Inspect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	approved := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	defer once.Do(func() { close(release) })
	writePolicy := func(deny bool) agenttools.Tool {
		return agenttools.Tool{Name: "write", Call: func(context.Context, agenttools.Call) (agenttools.Result, error) { return agenttools.Result{}, nil }, WriteAllowed: func(context.Context, string) error {
			if deny {
				return errors.New("new policy refuses writes")
			}
			close(approved)
			<-release
			return nil
		}}
	}
	unregister, err := r.kit.reg.Register(writePolicy(false))
	if err != nil {
		t.Fatal(err)
	}
	result := make(chan error, 1)
	go func() {
		cap := authorizedContext{Capability: c, runtime: r.rt, path: c.Path()}
		_, err := cap.Edit(t.Context(), contextkit.EditRequest{ExpectedRevision: info.Revision, Edits: []contextkit.Edit{{Text: "must not land"}}})
		result <- err
	}()
	contextReceive(t, approved)
	unregister()
	if _, err := r.kit.reg.Register(writePolicy(true)); err != nil {
		t.Fatal(err)
	}
	once.Do(func() { close(release) })
	if err := contextReceive(t, result); err == nil || !strings.Contains(err.Error(), "policy changed") {
		t.Fatalf("queued mutation ignored replaced policy: %v", err)
	}
	after, err := c.Inspect(t.Context())
	if err != nil || after.Revision != info.Revision {
		t.Fatalf("denied mutation changed context revision: %+v %v", after, err)
	}
}

type contextRewriteHooks struct{ args json.RawMessage }

func (h contextRewriteHooks) PreTool(context.Context, string, agenttools.Call, string) (json.RawMessage, string) {
	return h.args, ""
}
func (contextRewriteHooks) PostTool(_ context.Context, _ string, _ agenttools.Call, _ string, r agenttools.Result) agenttools.Result {
	return r
}

// The scope is bound before dispatch, but the path is chosen by the
// file tool after PreTool has rewritten the arguments.
func TestCLMHookRewrittenFileUsesOwningContext(t *testing.T) {
	t.Parallel()
	var rewrite contextRewriteHooks
	r := newRigWith(t, []rigOpt{clmMode, func(d *Deps) { d.Hooks = func() agenttools.Hooks { return rewrite } }})
	c, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(c.Path(), []byte("rewritten target"), 0600); err != nil {
		t.Fatal(err)
	}
	before, err := c.Inspect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	rewrite.args, _ = json.Marshal(map[string]string{"path": c.Path()})
	result := make(chan error, 1)
	_, err = r.kit.reg.Register(agenttools.Tool{Name: "write", Call: func(ctx context.Context, call agenttools.Call) (agenttools.Result, error) {
		var args struct{ Path string }
		if err := json.Unmarshal(call.Args, &args); err != nil {
			return agenttools.Result{}, err
		}
		path, err := agenttools.CanonicalFile(args.Path)
		if err == nil {
			err = agenttools.MutateFile(ctx, path, func() error { return os.WriteFile(path, []byte("rewritten target"), 0600) })
		}
		result <- err
		return agenttools.Result{}, err
	}})
	if err != nil {
		t.Fatal(err)
	}
	h := r.rt.newHandler(r.rt.gate, "", nil)
	defer h.Close()
	original := filepath.Join(r.dir, "unrelated.md")
	args, _ := json.Marshal(map[string]string{"path": original})
	contextToolCall(t, h, "rewritten", "write", string(args))
	if err := contextReceive(t, result); err != nil {
		t.Fatal(err)
	}
	after, err := c.Inspect(t.Context())
	if err != nil || before.Revision == after.Revision || before.ContentHash != after.ContentHash {
		t.Fatalf("rewritten managed mutation was not coordinated: %+v %v", after, err)
	}
	if _, err := os.Stat(original); !os.IsNotExist(err) {
		t.Fatalf("pre-hook path was written: %v", err)
	}
}

func TestCLMSimultaneousSameNamedChildrenGetOwnCapability(t *testing.T) {
	t.Parallel()
	r := newRigWith(t, []rigOpt{clmMode},
		fake.Step{Output: []ullmItem{fake.Call("inspect-a", "context_inspect", `{}`)}},
		fake.Step{Output: []ullmItem{fake.Call("inspect-b", "context_inspect", `{}`)}},
		fake.Step{Output: []ullmItem{fake.Text("child done")}},
		fake.Step{Output: []ullmItem{fake.Text("child done")}},
	)
	main, err := r.rt.gate.context()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(main.Path(), []byte("parent-only"), 0600); err != nil {
		t.Fatal(err)
	}
	entered := make(chan string, 2)
	release := make(chan struct{})
	var releaseOnce sync.Once
	defer releaseOnce.Do(func() { close(release) })
	_, err = r.kit.reg.Register(agenttools.Tool{Name: "context_inspect", RequiresContext: true, Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
		if c.Context == nil {
			return agenttools.Result{}, errors.New("missing child capability")
		}
		info, err := c.Context.Inspect(ctx)
		if err != nil {
			return agenttools.Result{}, err
		}
		read, err := c.Context.Read(ctx, contextkit.ReadRequest{SnapshotID: info.SnapshotID})
		if err != nil {
			return agenttools.Result{}, err
		}
		entered <- read.Text
		select {
		case <-release:
		case <-ctx.Done():
		}
		return agenttools.Result{Text: "inspection complete"}, ctx.Err()
	}})
	if err != nil {
		t.Fatal(err)
	}
	results := make(chan error, 2)
	for _, task := range []string{"child-one", "child-two"} {
		go func() {
			res, err := r.rt.Children().Run(t.Context(), ChildRequest{Task: task, Worker: "same", MaxSteps: 3})
			if err == nil && res.Status != "done" {
				err = errors.New(res.Status + ": " + res.Reply)
			}
			results <- err
		}()
	}
	a, b := contextReceive(t, entered), contextReceive(t, entered)
	if strings.Contains(a+b, "parent-only") || a == b || !strings.Contains(a+b, "child-one") || !strings.Contains(a+b, "child-two") {
		t.Fatalf("child capabilities were shared: %q / %q", a, b)
	}
	releaseOnce.Do(func() { close(release) })
	for range 2 {
		if err := contextReceive(t, results); err != nil {
			t.Fatal(err)
		}
	}
	bodies, _ := filepath.Glob(filepath.Join(r.dir, "scratch", ".bough-clm", "*.md"))
	if len(bodies) != 3 {
		t.Fatalf("want parent plus two child contexts, got %v", bodies)
	}
	parent, _ := os.ReadFile(main.Path())
	if string(parent) != "parent-only" {
		t.Fatalf("child changed parent: %s", parent)
	}
	r.rt.contextsMu.RLock()
	active := len(r.rt.contexts)
	r.rt.contextsMu.RUnlock()
	if active != 1 {
		t.Fatalf("finished children retained their context snapshots: %d active contexts", active)
	}
}
