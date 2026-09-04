package ui

import (
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/tools"
)

type stubJobs struct{ live []tools.Running }

func (s *stubJobs) Running() []tools.Running { return s.live }

func withJobs(t *testing.T, live ...tools.Running) model {
	t.Helper()
	m := testModel(t)
	cfg := m.cfg.Load()
	cfg.jobs = &stubJobs{live: live}
	m.cfg.Store(cfg)
	return m
}

// A job started with a time limit outlives its turn, and until it
// finishes nothing on screen says so. The strip under the composer is
// that standing answer.
func TestJobStripNamesRunningJobs(t *testing.T) {
	m := withJobs(t,
		tools.Running{ID: 1, Cmd: "go test ./...", Since: 134 * time.Second},
		tools.Running{ID: 2, Cmd: "npm run dev", Since: 90 * time.Minute, Watch: "listening on"},
	)
	strip := stripANSI(m.jobStrip(m.cfg.Load()))
	for _, want := range []string{"job 1", "go test ./...", "2m14s", "job 2", "npm run dev", "1h30m", "watching listening on"} {
		if !strings.Contains(strip, want) {
			t.Fatalf("strip missing %q:\n%s", want, strip)
		}
	}
	if n := strings.Count(strip, "\n"); n != 1 {
		t.Fatalf("want one row per job, got %d lines:\n%s", n+1, strip)
	}
}

// Nothing running, nothing shown — and no service at all is the same.
func TestJobStripEmpty(t *testing.T) {
	none := withJobs(t)
	if got := none.jobStrip(none.cfg.Load()); got != "" {
		t.Fatalf("strip with no jobs = %q", got)
	}
	m := testModel(t)
	if got := m.jobStrip(m.cfg.Load()); got != "" {
		t.Fatalf("strip with no service = %q", got)
	}
}

// Many jobs are counted rather than listed: the strip must not eat the
// transcript.
func TestJobStripCaps(t *testing.T) {
	var live []tools.Running
	for i := 1; i <= 7; i++ {
		live = append(live, tools.Running{ID: i, Cmd: "sleep 100", Since: time.Second})
	}
	many := withJobs(t, live...)
	strip := stripANSI(many.jobStrip(many.cfg.Load()))
	if lines := strings.Count(strip, "\n") + 1; lines != jobStripMax+1 {
		t.Fatalf("%d lines, want %d rows plus the count:\n%s", lines, jobStripMax, strip)
	}
	if !strings.Contains(strip, "and 4 more") {
		t.Fatalf("the rest were not counted:\n%s", strip)
	}
}

// The strip takes its rows from the transcript, like the status bar:
// the frame still fits the terminal.
func TestJobStripKeepsTheFrameInTheTerminal(t *testing.T) {
	m := withJobs(t,
		tools.Running{ID: 1, Cmd: "go build ./...", Since: time.Second},
		tools.Running{ID: 2, Cmd: "sleep 60", Since: time.Second},
	)
	m.resize(80, 24)
	m.addEvent(Event{Kind: "assistant", Text: strings.Repeat("filler\n", 40)})
	frame := m.frame()
	if got := strings.Count(frame, "\n") + 1; got > 24 {
		t.Fatalf("frame is %d lines for a 24-line terminal:\n%s", got, frame)
	}
	if !strings.Contains(stripANSI(frame), "go build ./...") {
		t.Fatalf("the strip is not in the frame:\n%s", frame)
	}
}

func TestShortDur(t *testing.T) {
	for d, want := range map[time.Duration]string{
		9 * time.Second:              "9s",
		59 * time.Second:             "59s",
		134 * time.Second:            "2m14s",
		time.Hour + 3*time.Minute:    "1h03m",
		2*time.Hour + 30*time.Minute: "2h30m",
	} {
		if got := shortDur(d); got != want {
			t.Errorf("shortDur(%s) = %q, want %q", d, got, want)
		}
	}
}
