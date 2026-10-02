//go:build !windows

package clm

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"testing/synctest"

	"github.com/andreylukin/bough/internal/contextkit"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

func inspect(t *testing.T, c *Context) contextkit.Info {
	t.Helper()
	i, e := c.Inspect(context.Background())
	if e != nil {
		t.Fatal(e)
	}
	return i
}
func live(t *testing.T, c *Context) string {
	t.Helper()
	b, e := os.ReadFile(c.path)
	if e != nil {
		t.Fatal(e)
	}
	return string(b)
}
func diskFiles(t *testing.T, root string) map[string]string {
	t.Helper()
	out := map[string]string{}
	e := filepath.WalkDir(root, func(p string, d os.DirEntry, e error) error {
		if e != nil {
			return e
		}
		if !d.IsDir() {
			b, e := os.ReadFile(p)
			if e != nil {
				return e
			}
			out[p] = string(b)
		}
		return nil
	})
	if e != nil {
		t.Fatal(e)
	}
	return out
}

func TestToolkitImmutableParallelSnapshots(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	replace(t, c, "α hit\nhit tail")
	i := inspect(t, c)
	start := make(chan struct{})
	results := make(chan error, 2)
	go func() {
		<-start
		r, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID, Limit: 2})
		if e == nil && (r.Text != "α" || !r.HasMore || r.Revision != i.Revision) {
			e = errors.New("read changed snapshot")
		}
		results <- e
	}()
	go func() {
		<-start
		r, e := c.Search(context.Background(), contextkit.SearchRequest{SnapshotID: i.SnapshotID, Query: "hit", Limit: 1})
		if e == nil && (len(r.Matches) != 1 || r.Matches[0].Start != 3 || !r.HasMore) {
			e = errors.New("search changed snapshot")
		}
		results <- e
	}()
	_, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: i.Bytes, Text: "different"}}})
	if e != nil {
		t.Fatal(e)
	}
	close(start)
	for range 2 {
		if e := <-results; e != nil {
			t.Fatal(e)
		}
	}
	r, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID, Offset: 2})
	if e != nil || r.Text != " hit\nhit tail" {
		t.Fatalf("page = %+v %v", r, e)
	}
	s, e := c.Search(context.Background(), contextkit.SearchRequest{SnapshotID: i.SnapshotID, Query: "hit", Offset: 6})
	if e != nil || len(s.Matches) != 1 || s.Matches[0].Line != 2 {
		t.Fatalf("search page = %+v %v", s, e)
	}
	for range snapshotRetention {
		inspect(t, c)
	}
	if _, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID}); !errors.Is(e, contextkit.ErrSnapshotExpired) {
		t.Fatalf("expiry = %v", e)
	}
	other := testContext(t, 0)
	if _, e := other.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID}); !errors.Is(e, contextkit.ErrForeignReference) {
		t.Fatalf("foreign = %v", e)
	}
}

func TestToolkitCASBatchDryRunAndABA(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	prepare(t, c, message(ullm.RoleUser, "original authenticated event"))
	replace(t, c, "abcdef")
	i := inspect(t, c)
	stateBefore := c.state
	before := diskFiles(t, filepath.Dir(c.path))
	bad := contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: 1, Text: "A"}, {Start: 99, End: 100, Text: "X"}}}
	if _, e := c.Edit(context.Background(), bad); e == nil {
		t.Fatal("invalid second edit committed")
	}
	good := contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: 1, Text: "XY"}, {Start: 2, End: 3, Text: "Z"}}, DryRun: true}
	r, e := c.Edit(context.Background(), good)
	if e != nil || r.Revision != i.Revision || r.ContentHash != hashText([]byte("XYZcdef")) {
		t.Fatalf("dry run %+v %v", r, e)
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) || !reflect.DeepEqual(stateBefore, c.state) {
		t.Fatal("validation/dry run mutated disk or cursor")
	}
	start := make(chan struct{})
	errs := make(chan error, 2)
	var wg sync.WaitGroup
	for _, v := range []string{"A", "B"} {
		wg.Go(func() {
			<-start
			_, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: 1, Text: v}}})
			errs <- e
		})
	}
	close(start)
	wg.Wait()
	close(errs)
	wins, conflicts := 0, 0
	for e := range errs {
		if e == nil {
			wins++
		} else if errors.Is(e, contextkit.ErrConflict) {
			conflicts++
		} else {
			t.Fatal(e)
		}
	}
	if wins != 1 || conflicts != 1 {
		t.Fatalf("wins=%d conflicts=%d", wins, conflicts)
	}
	now := inspect(t, c)
	if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: now.Revision, Edits: []contextkit.Edit{{Start: 0, End: 1, Text: "a"}}}); e != nil {
		t.Fatal(e)
	}
	if live(t, c) != "abcdef" {
		t.Fatal("ABA setup failed")
	}
	if _, e := c.Edit(context.Background(), good); !errors.Is(e, contextkit.ErrConflict) {
		t.Fatalf("ABA accepted stale token: %v", e)
	}
	if !reflect.DeepEqual(stateBefore.Seen, c.state.Seen) || !reflect.DeepEqual(stateBefore.Delivered, c.state.Delivered) {
		t.Fatal("edits rewound event cursor")
	}
}

func TestToolkitQueuedMutationCancellationAndUnrelatedPath(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		c := testContext(t, 0)
		replace(t, c, "before")
		i := inspect(t, c)
		entered, release := make(chan struct{}), make(chan struct{})
		finished := make(chan error, 1)
		go func() {
			_, e := c.MutateFile(context.Background(), c.path, func() error { close(entered); <-release; return os.WriteFile(c.path, []byte("after"), 0600) })
			finished <- e
		}()
		<-entered
		read, err := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID})
		if err != nil || read.Text != "before" {
			t.Fatalf("saved read waited for/mixed with mutation: %+v %v", read, err)
		}
		search, err := c.Search(context.Background(), contextkit.SearchRequest{SnapshotID: i.SnapshotID, Query: "before"})
		if err != nil || len(search.Matches) != 1 {
			t.Fatalf("saved search during mutation: %+v %v", search, err)
		}
		called := false
		managed, e := c.MutateFile(context.Background(), filepath.Join(filepath.Dir(c.path), "ordinary.txt"), func() error { called = true; return nil })
		if e != nil || managed || called {
			t.Fatal("unrelated path was locked or invoked")
		}
		ctx, cancel := context.WithCancel(context.Background())
		queued := make(chan error, 1)
		go func() { _, e := c.MutateFile(ctx, c.path, func() error { called = true; return nil }); queued <- e }()
		synctest.Wait()
		cancel()
		if e := <-queued; !errors.Is(e, context.Canceled) {
			t.Fatalf("queued cancellation = %v", e)
		}
		close(release)
		if e := <-finished; e != nil {
			t.Fatal(e)
		}
		if called || live(t, c) != "after" {
			t.Fatal("canceled callback ran")
		}
		if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: 0, Text: "lost"}}}); !errors.Is(e, contextkit.ErrConflict) {
			t.Fatalf("native write did not invalidate CAS: %v", e)
		}
	})
}

func TestToolkitOffloadRestoreForkAndCorruption(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	original := "α\r\nsame same\n[[clm-image:owned]]\n"
	replace(t, c, original)
	i := inspect(t, c)
	before := diskFiles(t, filepath.Dir(c.path))
	q := contextkit.OffloadRequest{ExpectedRevision: i.Revision, Start: 0, End: len(original), Replacement: stringPtr("notes"), DryRun: true}
	if _, e := c.Offload(context.Background(), q); e != nil {
		t.Fatal(e)
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) {
		t.Fatal("offload dry run wrote archive")
	}
	q.DryRun = false
	o, e := c.Offload(context.Background(), q)
	if e != nil || o.ArchiveID == "" {
		t.Fatalf("offload %+v %v", o, e)
	}
	other := testContext(t, 0)
	oi := inspect(t, other)
	if _, e := other.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: oi.Revision, ArchiveID: o.ArchiveID}); !errors.Is(e, contextkit.ErrForeignReference) {
		t.Fatalf("foreign archive = %v", e)
	}
	cp := filepath.Join(t.TempDir(), "fork.json")
	if e := c.Snapshot(cp); e != nil {
		t.Fatal(e)
	}
	d := t.TempDir()
	path, state := filepath.Join(d, "child.md"), filepath.Join(d, "cursor.json")
	if e := Restore(cp, path, state); e != nil {
		t.Fatal(e)
	}
	child, e := Open(path, state, 0)
	if e != nil {
		t.Fatal(e)
	}
	if e := os.Remove(c.archivePath(o.ArchiveID)); e != nil {
		t.Fatal(e)
	}
	if e := child.Append([]ullm.Item{message(ullm.RoleAssistant, "new event")}); e != nil {
		t.Fatal(e)
	}
	ci := inspect(t, child)
	prior := live(t, child)
	if _, e := child.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: ci.Revision, ArchiveID: o.ArchiveID, Offset: ci.Bytes}); e != nil {
		t.Fatal(e)
	}
	if got := live(t, child); got != prior+original {
		t.Fatalf("restore lost bytes: %q", got)
	}
	if live(t, c) != "notes" {
		t.Fatal("child changed parent")
	}
	if e := os.WriteFile(child.archivePath(o.ArchiveID), []byte("corrupt"), 0600); e != nil {
		t.Fatal(e)
	}
	ci = inspect(t, child)
	prior = live(t, child)
	if _, e := child.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: ci.Revision, ArchiveID: o.ArchiveID}); e == nil {
		t.Fatal("corrupt archive accepted")
	}
	if live(t, child) != prior {
		t.Fatal("corrupt restore mutated body")
	}
}

func TestToolkitPendingRecoveryIsReadOnlyAndNoLoss(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	replace(t, c, "keep excerpt")
	i := inspect(t, c)
	state, e := os.ReadFile(c.statePath)
	if e != nil {
		t.Fatal(e)
	}
	// Force the transaction's final cursor rename to fail after the archive
	// and new live body have become durable.
	if e := os.Remove(c.statePath); e != nil {
		t.Fatal(e)
	}
	if e := os.Mkdir(c.statePath, 0700); e != nil {
		t.Fatal(e)
	}
	_, e = c.Offload(context.Background(), contextkit.OffloadRequest{ExpectedRevision: i.Revision, Start: 5, End: 12, Replacement: stringPtr("")})
	if e == nil {
		t.Fatal("injected cursor failure succeeded")
	}
	before := diskFiles(t, filepath.Dir(c.path))
	if _, e := c.Inspect(context.Background()); !errors.Is(e, contextkit.ErrRecoveryRequired) {
		t.Fatalf("inspect = %v", e)
	}
	if r, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID}); e != nil || r.Text != "keep excerpt" {
		t.Fatalf("saved snapshot during recovery = %+v %v", r, e)
	}
	if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, DryRun: true}); !errors.Is(e, contextkit.ErrRecoveryRequired) {
		t.Fatalf("dry run = %v", e)
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) {
		t.Fatal("read-only operation repaired journal")
	}
	if e := os.Remove(c.statePath); e != nil {
		t.Fatal(e)
	}
	if e := os.WriteFile(c.statePath, state, 0600); e != nil {
		t.Fatal(e)
	}
	reopened, e := Open(c.path, c.statePath, 0)
	if e != nil {
		t.Fatal(e)
	}
	if live(t, reopened) != "keep " || len(reopened.state.Archives) != 1 {
		t.Fatal("recovery lost offload manifest")
	}
	for id := range reopened.state.Archives {
		j := inspect(t, reopened)
		if _, e := reopened.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: j.Revision, ArchiveID: id, Offset: j.Bytes}); e != nil {
			t.Fatal(e)
		}
	}
	if live(t, reopened) != "keep excerpt" {
		t.Fatal("recovery lost excerpt bytes")
	}
}

func TestToolkitReceiptsPreserveFreshProtocolAndPreparedHash(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	prepare(t, c, message(ullm.RoleUser, "task"))
	call := ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "read-1", Name: "context_read", Arguments: `{"snapshot_id":"old"}`}}
	if e := c.Append([]ullm.Item{call}); e != nil {
		t.Fatal(e)
	}
	payload := strings.Repeat("copied-context-", 1000)
	result := ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "read-1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: payload}}}}
	r, hash, e := c.PrepareRevision(ullm.Request{Input: []ullm.Item{message(ullm.RoleUser, "task"), call, result}})
	if e != nil {
		t.Fatal(e)
	}
	body := live(t, c)
	if strings.Contains(body, "copied-context-") || len(body) > 1024 {
		t.Fatal("toolkit result recursively copied into notes")
	}
	if hash != hashText([]byte(body)) {
		t.Fatal("prepared content hash does not match body")
	}
	found := false
	for _, it := range r.Input {
		if d, ok := it.Data.(ullm.ToolResult); ok && d.CallID == "read-1" {
			found = d.Output[0].Value == payload
		}
	}
	if !found {
		t.Fatal("fresh native result protocol was truncated")
	}
	beforeSeen, _ := json.Marshal(c.state.Seen)
	i := inspect(t, c)
	if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: len(body), Text: "notes"}}}); e != nil {
		t.Fatal(e)
	}
	afterSeen, _ := json.Marshal(c.state.Seen)
	if string(beforeSeen) != string(afterSeen) {
		t.Fatal("edit changed seen cursor")
	}
	current, e := c.Revision()
	if e != nil || current == hash {
		t.Fatal("prepared hash followed later mutation")
	}
}

// The model cannot use an inspect result if recording that very result changes
// the editable revision before its next edit call arrives.
func TestToolkitInspectRevisionSurvivesItsOwnProtocol(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	user := message(ullm.RoleUser, "task")
	prepare(t, c, user)
	call := ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "inspect-1", Name: "context_inspect", Arguments: `{}`}}
	if e := c.Append([]ullm.Item{call}); e != nil {
		t.Fatal(e)
	}
	i := inspect(t, c)
	payload, _ := json.Marshal(i)
	result := ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "inspect-1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: string(payload)}}}}
	projected := prepare(t, c, user, call, result)
	if !strings.Contains(texts(projected), "Context toolkit receipts") {
		t.Fatal("projection omitted bounded receipt")
	}
	editCall := ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "edit-1", Name: "context_edit", Arguments: `{}`}}
	if e := c.Append([]ullm.Item{editCall}); e != nil {
		t.Fatal(e)
	}
	if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: i.Bytes, Text: "new notes"}}}); e != nil {
		t.Fatalf("own protocol invalidated inspect revision: %v", e)
	}
}

func TestToolkitCommitPolicyAndUTF8Bounds(t *testing.T) {
	t.Parallel()
	c := testContext(t, 16)
	replace(t, c, "α beta")
	i := inspect(t, c)
	before := diskFiles(t, filepath.Dir(c.path))
	denied := errors.New("write policy changed")
	ctx := contextkit.WithCommitCheck(context.Background(), func() error { return denied })
	q := contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: 2, Text: "A"}}}
	if _, e := c.Edit(ctx, q); !errors.Is(e, denied) {
		t.Fatalf("edit policy = %v", e)
	}
	if _, e := c.Offload(ctx, contextkit.OffloadRequest{ExpectedRevision: i.Revision, Start: 0, End: 2, Replacement: stringPtr("")}); !errors.Is(e, denied) {
		t.Fatalf("offload policy = %v", e)
	}
	called := false
	if _, e := c.MutateFile(ctx, c.path, func() error { called = true; return nil }); !errors.Is(e, denied) || called {
		t.Fatalf("native policy = %v called=%v", e, called)
	}
	q.DryRun = true
	if _, e := c.Edit(ctx, q); e != nil {
		t.Fatalf("dry run called commit check: %v", e)
	}
	for _, edits := range [][]contextkit.Edit{{{Start: 1, End: 2, Text: "x"}}, {{Start: 0, End: 0, Text: strings.Repeat("x", 17)}}, {{Start: 0, End: 0, Text: string([]byte{0xff})}}} {
		if _, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: edits}); e == nil {
			t.Fatal("invalid/oversized candidate accepted")
		}
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) {
		t.Fatal("failed/dry mutation changed disk")
	}
	alias := filepath.Join(filepath.Dir(c.path), "alias.md")
	if e := os.Symlink(c.path, alias); e != nil {
		t.Fatal(e)
	}
	managed, e := c.MutateFile(context.Background(), alias, func() error { called = true; return nil })
	if !managed || e == nil || called {
		t.Fatal("symlink alias was not rejected")
	}
}

func stringPtr(s string) *string { return &s }

func TestToolkitOffloadDefaultMarkerAndExplicitRemoval(t *testing.T) {
	t.Parallel()
	for _, replacement := range []*string{nil, stringPtr(""), stringPtr("chosen exactly")} {
		t.Run(func() string {
			if replacement == nil {
				return "default"
			}
			if *replacement == "" {
				return "empty"
			}
			return "custom"
		}(), func(t *testing.T) {
			t.Parallel()
			c := testContext(t, 0)
			replace(t, c, "original excerpt")
			i := inspect(t, c)
			o, e := c.Offload(context.Background(), contextkit.OffloadRequest{ExpectedRevision: i.Revision, Start: 0, End: i.Bytes, Replacement: replacement})
			if e != nil {
				t.Fatal(e)
			}
			want := "[[clm-archive:" + o.ArchiveID + "]]"
			if replacement != nil {
				want = *replacement
			}
			if live(t, c) != want {
				t.Fatalf("replacement = %q, want %q", live(t, c), want)
			}
			reopened, e := Open(c.path, c.statePath, 0)
			if e != nil {
				t.Fatal(e)
			}
			if live(t, reopened) != want {
				t.Fatal("reopen lost marker")
			}
			j := inspect(t, reopened)
			if _, e := reopened.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: j.Revision, ArchiveID: o.ArchiveID, Offset: j.Bytes}); e != nil {
				t.Fatal(e)
			}
			if live(t, reopened) != want+"original excerpt" {
				t.Fatal("reopen lost archive")
			}
		})
	}
}

func TestToolkitPageCapsAndOrderedBatchCommit(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	replace(t, c, strings.Repeat("x", readPageLimit+100))
	i := inspect(t, c)
	r, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID, Limit: readPageLimit * 10})
	if e != nil || len(r.Text) != readPageLimit || !r.HasMore {
		t.Fatalf("bounded read %+v %v", r, e)
	}
	s, e := c.Search(context.Background(), contextkit.SearchRequest{SnapshotID: i.SnapshotID, Query: "x", Limit: 1000})
	if e != nil || len(s.Matches) != searchPageLimit || !s.HasMore {
		t.Fatalf("bounded search count=%d %v", len(s.Matches), e)
	}
	j, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: i.Bytes, Text: "abc"}, {Start: 0, End: 1, Text: "XY"}, {Start: 2, End: 3, Text: "Z"}}})
	if e != nil || live(t, c) != "XYZc" || j.Revision == i.Revision {
		t.Fatalf("ordered batch %+v %v body=%q", j, e, live(t, c))
	}
	if c.state.Generation != 1 {
		t.Fatal("ordered batch published intermediate revisions")
	}
}

func TestToolkitLegacyCursorReadOnlyThenVersionedMutation(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	prepare(t, c, message(ullm.RoleUser, "legacy event"))
	replace(t, c, "legacy notes")
	legacy := c.state
	legacy.Version = 1
	legacy.Generation = 0
	legacy.Archives = nil
	legacy.ContextCalls = nil
	b, e := json.Marshal(legacy)
	if e != nil {
		t.Fatal(e)
	}
	if e := os.WriteFile(c.statePath, b, 0600); e != nil {
		t.Fatal(e)
	}
	before := diskFiles(t, filepath.Dir(c.path))
	old, e := Open(c.path, c.statePath, 0)
	if e != nil {
		t.Fatal(e)
	}
	i := inspect(t, old)
	if _, e := old.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID}); e != nil {
		t.Fatal(e)
	}
	if _, e := old.Search(context.Background(), contextkit.SearchRequest{SnapshotID: i.SnapshotID, Query: "legacy"}); e != nil {
		t.Fatal(e)
	}
	q := contextkit.EditRequest{ExpectedRevision: i.Revision, Edits: []contextkit.Edit{{Start: 0, End: i.Bytes, Text: "new notes"}}, DryRun: true}
	if _, e := old.Edit(context.Background(), q); e != nil {
		t.Fatal(e)
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) || old.state.Version != 1 {
		t.Fatal("read-only access migrated legacy state")
	}
	q.DryRun = false
	if _, e := old.Edit(context.Background(), q); e != nil {
		t.Fatal(e)
	}
	if old.state.Version != 2 || !reflect.DeepEqual(old.state.Seen, legacy.Seen) || !reflect.DeepEqual(old.state.Delivered, legacy.Delivered) {
		t.Fatal("migration lost cursor or failed to mark incompatible version")
	}
	data, e := os.ReadFile(old.statePath)
	if e != nil {
		t.Fatal(e)
	}
	var header struct{ Version int }
	if e := json.Unmarshal(data, &header); e != nil {
		t.Fatal(e)
	}
	if header.Version == 1 {
		t.Fatal("legacy reader would accept and drop new fields")
	}
	if _, e := Open(old.path, old.statePath, 0); e != nil {
		t.Fatal(e)
	}
	cp := filepath.Join(t.TempDir(), "snapshot.json")
	if e := old.Snapshot(cp); e != nil {
		t.Fatal(e)
	}
	data, e = os.ReadFile(cp)
	if e != nil {
		t.Fatal(e)
	}
	var saved checkpoint
	if e := json.Unmarshal(data, &saved); e != nil || saved.State.Version != 2 {
		t.Fatalf("new checkpoint version = %d %v", saved.State.Version, e)
	}
	// Historical version-1 fork checkpoints still restore into fresh v2 state.
	historical, _ := json.Marshal(checkpoint{State: legacy, Text: "historical notes"})
	if e := os.WriteFile(cp, historical, 0600); e != nil {
		t.Fatal(e)
	}
	d := t.TempDir()
	path, state := filepath.Join(d, "child.md"), filepath.Join(d, "child.json")
	if e := Restore(cp, path, state); e != nil {
		t.Fatal(e)
	}
	child, e := Open(path, state, 0)
	if e != nil || child.state.Version != 2 || live(t, child) != "historical notes" {
		t.Fatalf("historical restore = %v", e)
	}
	if !reflect.DeepEqual(child.state.Seen, legacy.Seen) || !reflect.DeepEqual(child.state.Delivered, legacy.Delivered) {
		t.Fatal("historical fork changed cursor")
	}
}

func TestToolkitArchiveJournalRecoveryPhases(t *testing.T) {
	t.Parallel()
	for _, phase := range []string{"before-file", "after-file", "after-cursor", "external-edit"} {
		t.Run(phase, func(t *testing.T) {
			t.Parallel()
			c := testContext(t, 0)
			replace(t, c, "prefix excerpt")
			id := c.owner() + "-" + hashText([]byte("excerpt"))
			if e := c.saveArchive(id, "excerpt"); e != nil {
				t.Fatal(e)
			}
			next := c.state
			next.Generation++
			next.Archives = map[string]string{id: hashText([]byte("excerpt"))}
			tx := transaction{Before: "prefix excerpt", After: "prefix ", Next: next}
			data, _ := json.Marshal(tx)
			if e := atomicWrite(c.statePath+".pending", data); e != nil {
				t.Fatal(e)
			}
			if phase != "before-file" {
				replace(t, c, tx.After)
			}
			if phase == "after-cursor" {
				data, _ = json.Marshal(next)
				if e := atomicWrite(c.statePath, data); e != nil {
					t.Fatal(e)
				}
			}
			if phase == "external-edit" {
				replace(t, c, "external edit")
				before := diskFiles(t, filepath.Dir(c.path))
				if _, e := Open(c.path, c.statePath, 0); e == nil {
					t.Fatal("conflicting recovery overwrote external edit")
				}
				if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) {
					t.Fatal("failed recovery changed either version")
				}
				return
			}
			reopened, e := Open(c.path, c.statePath, 0)
			if e != nil {
				t.Fatal(e)
			}
			if phase == "before-file" {
				if live(t, reopened) != "prefix excerpt" || len(reopened.state.Archives) != 0 {
					t.Fatal("aborted offload lost original bytes or committed manifest")
				}
				return
			}
			i := inspect(t, reopened)
			if _, e := reopened.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: i.Revision, ArchiveID: id, Offset: i.Bytes}); e != nil {
				t.Fatal(e)
			}
			if live(t, reopened) != "prefix excerpt" {
				t.Fatal("completed offload recovery lost bytes")
			}
		})
	}
}

func TestToolkitEncodedReadPagination(t *testing.T) {
	t.Parallel()
	for name, unit := range map[string]string{
		"control": "\x00\x01\x1f\b\f\n\r\t\\\"",
		"html":    "<tag attr=\"value\">&text</tag>",
		"unicode": "α世界🙂\u2028\u2029",
		"mixed":   "\x00<α&🙂>\u2028\n",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			c := testContext(t, 0)
			text := strings.Repeat(unit, 5000)
			replace(t, c, text)
			i := inspect(t, c)
			var joined strings.Builder
			for offset := 0; offset < len(text); {
				r, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: i.SnapshotID, Offset: offset, Limit: readPageLimit})
				if e != nil {
					t.Fatal(e)
				}
				if r.Offset != offset || r.NextOffset != offset+len(r.Text) || r.NextOffset <= offset || !boundary(text, r.NextOffset) {
					t.Fatalf("noncontiguous UTF-8 page: offset=%d next=%d length=%d", r.Offset, r.NextOffset, len(r.Text))
				}
				if len(r.Text) > readPageLimit {
					t.Fatal("raw page limit exceeded")
				}
				escaped, e := json.Marshal(r.Text)
				if e != nil || len(escaped) > readEncodedTextLimit {
					t.Fatalf("encoded text size=%d %v", len(escaped), e)
				}
				encoded, e := json.Marshal(r)
				if e != nil || len(encoded) >= 40000 {
					t.Fatalf("metadata/result exceeded default native output budget: size=%d %v", len(encoded), e)
				}
				var decoded contextkit.ReadResult
				if e := json.Unmarshal(encoded, &decoded); e != nil || decoded.Text != r.Text {
					t.Fatal("encoded response did not round trip")
				}
				joined.WriteString(decoded.Text)
				offset = r.NextOffset
				if r.HasMore != (offset < len(text)) {
					t.Fatal("incorrect final-page flag")
				}
			}
			if joined.String() != text {
				t.Fatal("pagination dropped or repeated bytes")
			}
		})
	}
}

func TestToolkitOpaqueMetadataErrorsStayBounded(t *testing.T) {
	t.Parallel()
	c := testContext(t, 0)
	replace(t, c, "notes")
	i := inspect(t, c)
	huge := strings.Repeat("<\x00&", 1<<16)
	checks := []struct {
		name string
		call func() error
	}{
		{"edit revision", func() error {
			_, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: huge, Edits: []contextkit.Edit{{Start: 0, End: 0, Text: "x"}}})
			return e
		}},
		{"offload revision", func() error {
			_, e := c.Offload(context.Background(), contextkit.OffloadRequest{ExpectedRevision: huge, Start: 0, End: 1})
			return e
		}},
		{"restore revision", func() error {
			_, e := c.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: huge, ArchiveID: c.owner() + "-" + hashText([]byte("x"))})
			return e
		}},
		{"snapshot", func() error {
			_, e := c.Read(context.Background(), contextkit.ReadRequest{SnapshotID: c.owner() + ":" + huge})
			return e
		}},
		{"search snapshot", func() error {
			_, e := c.Search(context.Background(), contextkit.SearchRequest{SnapshotID: huge, Query: "notes"})
			return e
		}},
		{"archive", func() error {
			_, e := c.Restore(context.Background(), contextkit.RestoreRequest{ExpectedRevision: i.Revision, ArchiveID: huge})
			return e
		}},
	}
	before := diskFiles(t, filepath.Dir(c.path))
	for _, check := range checks {
		t.Run(check.name, func(t *testing.T) {
			e := check.call()
			if e == nil {
				t.Fatal("malformed reference accepted")
			}
			if len(e.Error()) > 512 {
				t.Fatal("unbounded opaque value echoed in error")
			}
			var conflict *contextkit.Conflict
			if errors.As(e, &conflict) {
				b, _ := json.Marshal(conflict)
				if len(b) > 4096 {
					t.Fatal("unbounded structured conflict")
				}
			}
		})
	}
	// Even the largest accepted malformed token has a bounded JSON error.
	_, e := c.Edit(context.Background(), contextkit.EditRequest{ExpectedRevision: strings.Repeat("\x00", opaqueTokenLimit), Edits: []contextkit.Edit{{Start: 0, End: 0, Text: "x"}}})
	var conflict *contextkit.Conflict
	if !errors.As(e, &conflict) {
		t.Fatalf("expected bounded structured conflict, got %v", e)
	}
	payload, _ := json.Marshal(map[string]any{"message": e.Error(), "expected_revision": conflict.Expected, "current_revision": conflict.Current})
	if len(payload) > 4096 {
		t.Fatalf("bounded conflict expanded to %d bytes", len(payload))
	}
	if !reflect.DeepEqual(before, diskFiles(t, filepath.Dir(c.path))) {
		t.Fatal("invalid metadata mutated context")
	}
}

func TestToolkitForkRejectsMalformedArchiveSetBeforeWriting(t *testing.T) {
	t.Parallel()
	owner := strings.Repeat("a", 24)
	validID := owner + "-" + hashText([]byte("valid excerpt"))
	extraID := owner + "-" + hashText([]byte("extra excerpt"))
	for _, kind := range []string{"valid-plus-extra", "equal-size-wrong-reference", "corrupt-excerpt"} {
		t.Run(kind, func(t *testing.T) {
			t.Parallel()
			cp := checkpoint{
				State: diskState{Version: stateVersion, Path: "parent.md", Seen: map[string]int{}, Delivered: map[string]int{}, Archives: map[string]string{validID: hashText([]byte("valid excerpt"))}},
				Text:  "fork notes", Archives: map[string]string{validID: "valid excerpt"},
			}
			switch kind {
			case "valid-plus-extra":
				cp.Archives[extraID] = "extra excerpt"
			case "equal-size-wrong-reference":
				delete(cp.Archives, validID)
				cp.Archives[extraID] = "extra excerpt"
			case "corrupt-excerpt":
				cp.Archives[validID] = "changed excerpt"
			}
			data, e := json.Marshal(cp)
			if e != nil {
				t.Fatal(e)
			}
			snapshot := filepath.Join(t.TempDir(), "malformed.json")
			if e := os.WriteFile(snapshot, data, 0600); e != nil {
				t.Fatal(e)
			}
			destination := t.TempDir()
			path := filepath.Join(destination, "notes", "child.md")
			state := filepath.Join(destination, "private", "child.json")
			if e := Restore(snapshot, path, state); e == nil {
				t.Fatal("malformed checkpoint accepted")
			}
			entries, e := os.ReadDir(destination)
			if e != nil {
				t.Fatal(e)
			}
			if len(entries) != 0 {
				t.Fatalf("rejected checkpoint created destination files/directories: %v", entries)
			}
		})
	}
}
