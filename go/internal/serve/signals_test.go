package serve

import (
	"testing"

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
