package ui

// The cache chip on resume, driven from recorded tapes
// (internal/vtreal/testdata/replay/cache-chip*.jsonl) whose entries
// are re-stamped relative to now at test time. In-process rather than
// on the PTY: the replay llm reports no usage, so the cost row never
// mounts there and the bar has no chip to show.

import (
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

const cacheChipDir = "../../internal/vtreal/testdata/replay/"

// cacheChipResume loads tape, shifts it so its last done entry is age
// old, and resumes it with the usage its done entries recorded.
func cacheChipResume(t *testing.T, tape string, age time.Duration) *drv {
	t.Helper()
	entries, err := history.Read(cacheChipDir + tape)
	if err != nil {
		t.Fatal(err)
	}
	last := -1
	for i, e := range entries {
		if e.Kind == "done" {
			last = i
		}
	}
	if last < 0 {
		t.Fatalf("%s: no done entry", tape)
	}
	shift := time.Now().Add(-age).Sub(entries[last].At)
	for i := range entries {
		entries[i].At = entries[i].At.Add(shift)
	}
	cfg := cfgWith(t, nil, nil, fakeHist{path: "/tmp/cache-chip.jsonl", entries: entries})
	cfg.usage = fakeUsage{loop.SumUsage(entries)}
	return newDrv(t, 120, 30, cfg)
}

func TestCacheChip(t *testing.T) {
	t.Parallel()
	t.Run("TestCacheChipFreshResumeHot", func(t *testing.T) {
		t.Parallel()
		d := cacheChipResume(t, "cache-chip.jsonl", time.Minute)
		if p := d.plain(); !strings.Contains(p, "⚡ cache hot") || strings.Contains(p, "cache cold") {
			t.Errorf("resumed a minute after the last turn: want hot\n%s", p)
		}
		if d.m.cacheTick() == nil {
			t.Errorf("an open window must schedule its expiry tick\n%s", d.plain())
		}
	})
	t.Run("TestCacheChipOldResumeCold", func(t *testing.T) {
		t.Parallel()
		d := cacheChipResume(t, "cache-chip.jsonl", 26*time.Hour)
		if p := d.plain(); !strings.Contains(p, "❄ cache cold") || strings.Contains(p, "cache hot") {
			t.Errorf("resumed a day later: want cold\n%s", p)
		}
		if d.m.cacheTick() != nil {
			t.Errorf("a closed window schedules no tick\n%s", d.plain())
		}
	})
	t.Run("TestCacheChipJustPastTTLCold", func(t *testing.T) {
		t.Parallel()
		d := cacheChipResume(t, "cache-chip.jsonl", cacheTTL+time.Second)
		if p := d.plain(); !strings.Contains(p, "❄ cache cold") {
			t.Errorf("resumed a second past the window: want cold\n%s", p)
		}
	})
	t.Run("TestCacheChipNoCacheTokensNoChip", func(t *testing.T) {
		t.Parallel()
		d := cacheChipResume(t, "cache-chip-nocache.jsonl", time.Minute)
		if p := d.plain(); strings.Contains(p, "cache hot") || strings.Contains(p, "cache cold") {
			t.Errorf("a tape with no cache tokens: want no chip\n%s", p)
		}
	})
	t.Run("TestCacheChipTickExpires", func(t *testing.T) {
		t.Parallel()
		// 0.5 s left in the window; the tick fires a second after it
		// closes, so this waits about 1.5 s.
		d := cacheChipResume(t, "cache-chip.jsonl", cacheTTL-500*time.Millisecond)
		if p := d.plain(); !strings.Contains(p, "⚡ cache hot") {
			t.Fatalf("0.5 s before the window closes: want hot\n%s", p)
		}
		cmd := d.m.cacheTick()
		if cmd == nil {
			t.Fatalf("no expiry tick scheduled\n%s", d.plain())
		}
		start := time.Now()
		msg := cmd()
		if _, ok := msg.(cacheTickMsg); !ok {
			t.Fatalf("tick delivered %T, want cacheTickMsg\n%s", msg, d.plain())
		}
		if el := time.Since(start); el > 2*time.Second {
			t.Errorf("tick took %v, want about 1.5 s\n%s", el, d.plain())
		}
		d.feed(msg)
		if p := d.plain(); !strings.Contains(p, "❄ cache cold") || strings.Contains(p, "cache hot") {
			t.Errorf("after the expiry tick: want cold\n%s", p)
		}
	})
}
