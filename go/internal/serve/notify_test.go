package serve

import (
	"errors"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A live child gets the notice as a raw {"notice"} stdin line (the fake
// child echoes what it read) and the browser sees a notice event.
func TestNotifyLiveWritesLine(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-live")
	if err := f.sup.Send("sess-live", "hi"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	waitFor(t, "the first turn", func() bool { return hasKind(f.sup.Recent("sess-live"), "done") })
	if err := f.sup.Notify("sess-live", "agent done"); err != nil {
		t.Fatalf("Notify: %v", err)
	}
	waitFor(t, "the notice line", func() bool {
		for _, ev := range f.sup.Recent("sess-live") {
			if ev.Kind == "input" && ev.Text == `{"notice":"agent done"}` {
				return true
			}
		}
		return false
	})
	if !hasKind(f.sup.Recent("sess-live"), "notice") {
		t.Fatalf("no notice event: %v", kinds(f.sup.Recent("sess-live")))
	}
	es, _ := history.ReadFile(filepath.Join(f.hist, "sess-live.jsonl"))
	for _, e := range es {
		if e.Kind == "notice" {
			t.Fatalf("live notice also stored: %+v", e)
		}
	}
}

// A session with no process gets a stored notice and is not started.
func TestNotifyStoppedAppendsEntry(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "sess-stopped")
	if err := f.sup.Notify("sess-stopped", "report"); err != nil {
		t.Fatalf("Notify: %v", err)
	}
	es, err := history.ReadFile(filepath.Join(f.hist, "sess-stopped.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	last := es[len(es)-1]
	if last.Kind != "notice" || last.Data["to"] != "sess-stopped" || last.Data["text"] != "report" || last.Data["id"] == "" || last.Seq != 2 {
		t.Fatalf("last entry = %+v", last)
	}
	time.Sleep(100 * time.Millisecond)
	if n := f.startCount(t); n != 0 || f.sup.Live("sess-stopped") {
		t.Fatalf("Notify started the session (%d starts)", n)
	}
}

func TestNotifyUnknownSession(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	if err := f.sup.Notify("nope", "x"); !errors.Is(err, ErrUnknownSession) {
		t.Fatalf("Notify = %v, want ErrUnknownSession", err)
	}
}
