package serve

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestRunningJobs(t *testing.T) {
	t.Parallel()
	started := func(seq int64, id string, cmd string) history.Entry {
		return ent(seq, "result", text("job "+id+" started in the background (limit 30m0s): "+cmd+"\nYou will be told when it finishes"))
	}
	es := entries(
		started(1, "1", "go test ./..."),
		started(2, "2", "npm run dev"),
		ent(3, "job", text("job 1 [exited 0] go test ./... (3s)\nok")),
	)
	got := RunningJobs(es, true)
	if len(got) != 1 || got[0].ID != 2 || got[0].Cmd != "npm run dev" {
		t.Fatalf("running = %+v, want only job 2", got)
	}
	if RunningJobs(es, false) != nil {
		t.Fatal("a dead child's jobs died with it")
	}
	// A restarted child reuses id 1.
	es = append(es, started(4, "1", "make"))
	if got := RunningJobs(es, true); len(got) != 2 || got[1].Cmd != "make" {
		t.Fatalf("reused id = %+v", got)
	}
}

func TestLastCache(t *testing.T) {
	t.Parallel()
	usage := func(seq int64, read float64) history.Entry {
		return ent(seq, "done", map[string]any{"usage": map[string]any{"in": 1000.0, "cache_read": read}})
	}
	if LastCache(entries(usage(1, 0)), "") != nil {
		t.Fatal("a provider with no cache tokens has no cache")
	}
	c := LastCache(entries(usage(1, 800), usage(2, 0), ent(3, "input", text("hi"))), "anthropic/claude-sonnet-5")
	if c == nil || c.Read != 800 || c.At != ent(1, "", nil).At {
		t.Fatalf("cache = %+v, want the turn that reported one", c)
	}
}

func TestTroubled(t *testing.T) {
	t.Parallel()
	es := entries(ent(1, "input", text("hi")), ent(2, "error", text("boom")))
	now := es[1].At.Add(time.Hour)
	if !Troubled(StatusError, es, 0, now) {
		t.Fatal("an unseen failure needs a person")
	}
	if Troubled(StatusError, es, 2, now) {
		t.Fatal("a failure marked seen does not")
	}
	if !Troubled(StatusInterrupted, append(es, ent(3, "input", text("again"))), 2, now) {
		t.Fatal("something recorded after the ack resurfaces")
	}
	if Troubled(StatusStopped, es, 0, now) || Troubled(StatusDone, es, 0, now) {
		t.Fatal("a stop on purpose or a finished turn is not trouble")
	}
	ran := func(seq int64, cmd string, exit int) []history.Entry {
		return entries(ent(seq, "code", text(`tools.bash("`+cmd+`")`)), ent(seq+1, "result", map[string]any{"text": "", "exit": exit}))
	}
	failing := append(ran(1, "go test ./...", 1), ent(3, "done", nil))
	if !Troubled(StatusDone, failing, 0, now) {
		t.Fatal("a finished turn whose tests failed needs a person")
	}
	fixed := append(failing, ran(4, "go test ./...", 0)...)
	if Troubled(StatusDone, fixed, 0, now) {
		t.Fatal("a later passing run clears it")
	}
	if Troubled(StatusDone, append(ran(1, "ls", 2), ent(3, "done", nil)), 0, now) {
		t.Fatal("a failing non-test command is not a test failure")
	}
	if Troubled(StatusError, es, 0, now.Add(8*24*time.Hour)) {
		t.Fatal("a failure older than the window is history, not a queue item")
	}
}

func TestCacheTTLFor(t *testing.T) {
	t.Parallel()
	for model, want := range map[string]int{
		"anthropic/claude-opus-5": 300, "openai/gpt-6-astra": 1800, "~openai/gpt-5.6": 1800,
		"openai/gpt-5.4": 300, "": 300,
	} {
		if got := int(cacheTTLFor(model).Seconds()); got != want {
			t.Errorf("cacheTTLFor(%q) = %ds, want %ds", model, got, want)
		}
	}
}
