package serve

import (
	"slices"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestPendingRowsIncludeHistoryWrittenAfterListSnapshot(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	const id = "child-in-snapshot-gap"
	f.sup.mu.Lock()
	f.sup.kids[id] = &child{id: id}
	f.sup.meta[id] = SessionMeta{SpawnedBy: "parent", Project: "project"}
	f.sup.mu.Unlock()
	t.Cleanup(func() {
		f.sup.mu.Lock()
		delete(f.sup.kids, id)
		f.sup.mu.Unlock()
	})

	// Both list endpoints read history before filling in pending rows.
	// Let the child's first history land in the gap between those reads.
	infos, err := f.sup.List()
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, in := range infos {
		seen[in.ID] = true
	}
	f.seed(t, id, history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{
		"cwd": "/work", "mode": "project", "project": "project", "spawned_by": "parent",
	}})
	rows := f.api.pendingRows(seen)
	if len(rows) != 1 || rows[0].ID != id {
		t.Fatalf("child vanished between the history snapshot and pending rows: %+v", rows)
	}
	if rows[0].Starting || rows[0].Cwd != "/work" || rows[0].Project != "project" {
		t.Fatalf("persisted child still rendered as a placeholder: %+v", rows[0])
	}
	if slices.Contains(f.sup.startingIDs(), id) {
		t.Fatal("the single-session lookup still considers persisted history missing")
	}
	if seen[id] {
		t.Fatal("pending rows changed the caller's history snapshot")
	}
	seen[id] = true
	if rows := f.api.pendingRows(seen); len(rows) != 0 {
		t.Fatalf("child already in the snapshot was duplicated: %+v", rows)
	}
}

func TestPendingRowsKeepChildStateFilters(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name                         string
		live, queued, archived, seen bool
		parent                       string
		want                         int
	}{
		{name: "booting", live: true, parent: "parent", want: 1},
		{name: "died before history", parent: "parent"},
		{name: "archived", live: true, parent: "parent", archived: true},
		{name: "parentless", live: true},
		{name: "queued", queued: true, parent: "parent", want: 1},
		{name: "queue to live overlap", live: true, queued: true, parent: "parent", want: 1},
		{name: "already listed", live: true, parent: "parent", seen: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			f := newAPI(t)
			const id = "pending-child"
			f.sup.mu.Lock()
			f.sup.meta[id] = SessionMeta{SpawnedBy: tc.parent, Archived: tc.archived}
			if tc.live {
				f.sup.kids[id] = &child{id: id}
			}
			if tc.queued {
				// Model the same ID sampled before and after queue drain.
				f.sup.queue = append(f.sup.queue, queuedChild{id: id})
			}
			f.sup.mu.Unlock()
			t.Cleanup(func() {
				f.sup.mu.Lock()
				delete(f.sup.kids, id)
				f.sup.queue = nil
				f.sup.mu.Unlock()
			})
			rows := f.api.pendingRows(map[string]bool{id: tc.seen})
			if len(rows) != tc.want {
				t.Fatalf("pending rows = %+v, want %d", rows, tc.want)
			}
			if tc.want == 1 && (rows[0].ID != id || rows[0].Queued != tc.queued || rows[0].Starting == tc.queued) {
				t.Fatalf("incorrect pending state: %+v", rows[0])
			}
		})
	}
}
