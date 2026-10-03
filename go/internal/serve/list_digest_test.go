package serve

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestListSharesMetadataSnapshotWithDigest(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now().UTC()
	f.seed(t, "s", history.Entry{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hello"}})
	var observed *rowDigest
	infos, err := f.sup.list(func(in history.SessionInfo, entries []history.Entry) { observed = f.api.cacheDigest(in, entries) })
	if err != nil || len(infos) != 1 || observed == nil {
		t.Fatalf("list: %+v %v digest=%v", infos, err, observed)
	}
	if got := f.api.digest(infos[0]); got != observed {
		t.Fatal("list snapshot was parsed a second time for its digest")
	}
	entries, err := f.sup.Entries("s")
	if err != nil {
		t.Fatal(err)
	}
	if want := digestOf(entries, infos[0].ModTime); !reflect.DeepEqual(observed, want) {
		t.Fatalf("shared=%+v direct=%+v", observed, want)
	}
	// Liveness stays outside the shared parse, just as it does for a
	// digest obtained through the single-session fallback.
	live, _ := StatusOf(entries, true)
	dead, _ := StatusOf(entries, false)
	if observed.statusLive != live || observed.statusDead != dead {
		t.Fatal("shared digest changed liveness answers")
	}
}

func TestAPIListSharedDigestMatchesDirectAfterChanges(t *testing.T) {
	t.Parallel()
	for _, prewarm := range []bool{false, true} {
		t.Run(map[bool]string{false: "cold", true: "metadata-already-warm"}[prewarm], func(t *testing.T) {
			t.Parallel()
			f := newAPI(t)
			now := time.Now().UTC()
			f.seed(t, "s", history.Entry{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hello"}})
			if prewarm {
				if _, err := f.sup.List(); err != nil {
					t.Fatal(err)
				}
			}
			check := func() {
				t.Helper()
				code, body := f.do(t, "GET", "/api/sessions", "")
				if code != 200 || len(sessionIDs(t, body)) != 1 {
					t.Fatalf("response: %d %v", code, body)
				}
				infos, err := f.sup.List()
				if err != nil || len(infos) != 1 {
					t.Fatalf("list: %v %v", infos, err)
				}
				entries, err := f.sup.Entries("s")
				if err != nil {
					t.Fatal(err)
				}
				if got, want := f.api.digest(infos[0]), digestOf(entries, infos[0].ModTime); !reflect.DeepEqual(got, want) {
					t.Fatalf("cached=%+v direct=%+v", got, want)
				}
			}
			check()
			check()
			appendText := func(s string) {
				t.Helper()
				fd, err := os.OpenFile(filepath.Join(f.hist, "s.jsonl"), os.O_APPEND|os.O_WRONLY, 0600)
				if err != nil {
					t.Fatal(err)
				}
				defer fd.Close()
				if _, err = fd.WriteString(s); err != nil {
					t.Fatal(err)
				}
			}
			appendText("bad line\n{\"seq\":2,\"kind\":\"done\",\"data\":{}}\n{\"seq\":3,\"kind\":\"title\",\"data\":{\"text\":\"new title\"}}\n{\"seq\":4,\"kind\":\"input\",\"data\":{\"text\":\"par")
			check()
			check()
			appendText("tial\"}}\n")
			check()
			check()
		})
	}
}

// Another request can publish a newer digest while a cold read is still
// deriving its own. The cache key must make the next list reject the
// older snapshot, even when its observer finishes last.
func TestAPIListRecoversAfterOlderReadObserverFinishes(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now().UTC()
	first := history.Entry{Seq: 1, Kind: "input", At: now, Data: map[string]any{"text": "hello"}}
	f.seed(t, "s", first)
	ready := make(chan struct{})
	release := make(chan struct{})
	finished := make(chan error, 1)
	go func() {
		_, err := f.sup.list(func(in history.SessionInfo, entries []history.Entry) {
			close(ready)
			<-release
			f.api.cacheDigest(in, entries)
		})
		finished <- err
	}()
	<-ready
	f.seed(t, "s", history.Entry{Seq: 2, Kind: "done", At: now.Add(time.Second)}, history.Entry{Seq: 3, Kind: "title", At: now.Add(time.Second), Data: map[string]any{"text": "new title"}})
	code, body := f.do(t, "GET", "/api/sessions", "")
	close(release)
	if err := <-finished; err != nil {
		t.Fatal(err)
	}
	if code != 200 || len(sessionIDs(t, body)) != 1 {
		t.Fatalf("concurrent response: %d %v", code, body)
	}
	code, body = f.do(t, "GET", "/api/sessions", "")
	if code != 200 || len(sessionIDs(t, body)) != 1 {
		t.Fatalf("next response: %d %v", code, body)
	}
	in, ok, err := history.Lookup(f.hist, "s")
	if err != nil || !ok {
		t.Fatalf("lookup: %v %v", ok, err)
	}
	entries, err := f.sup.Entries("s")
	if err != nil {
		t.Fatal(err)
	}
	if got, want := f.api.digest(in), digestOf(entries, in.ModTime); !reflect.DeepEqual(got, want) {
		t.Fatalf("stale digest: got=%+v want=%+v", got, want)
	}
	if in.Title != "new title" || in.Entries != 3 {
		t.Fatalf("stale metadata: %+v", in)
	}
}
