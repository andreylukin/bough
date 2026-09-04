package ui

// The background-job strip: the rows under the composer naming what is
// still running. A job started with a time limit outlives its turn
// (that is the point), and until it finishes the transcript says
// nothing about it — so a build you kicked off two prompts ago is
// invisible while you type the next one. The strip is the standing
// answer to "what is bough still doing for me".

import (
	"fmt"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/tools"
)

// jobStripMax is how many jobs get a row of their own; the rest are
// counted. Three is enough to see a build, a test run and a server
// without the strip eating the transcript.
const jobStripMax = 3

// jobLister is the "job-notices" service's live half.
type jobLister interface{ Running() []tools.Running }

// jobRows renders the strip, one row per running job, newest last.
// Empty (and zero height) when nothing is running.
func (m *model) jobRows(cfg *uiCfg) []string {
	if cfg.jobs == nil {
		return nil
	}
	live := cfg.jobs.Running()
	if len(live) == 0 {
		return nil
	}
	th := cfg.theme
	shown := live
	if len(shown) > jobStripMax {
		shown = shown[:jobStripMax]
	}
	out := make([]string, 0, len(shown)+1)
	for _, r := range shown {
		mark := th["accent"].Render(m.spin.View())
		label := fmt.Sprintf("job %d", r.ID)
		tail := shortDur(r.Since)
		if r.Watch != "" {
			tail += " · watching " + r.Watch
		}
		// The command is what identifies the job, so it takes whatever
		// width is left after the fixed parts.
		room := max(m.width-len(label)-len(tail)-8, 12)
		row := mark + " " + th["dim"].Render(label+" · ") + line(r.Cmd, room) +
			th["dim"].Render(" · "+tail)
		out = append(out, row)
	}
	if n := len(live) - len(shown); n > 0 {
		out = append(out, th["dim"].Render(fmt.Sprintf("  … and %d more (tools.jobs)", n)))
	}
	return out
}

// shortDur is an elapsed time the width of a chip: 9s, 2m14s, 1h03m.
func shortDur(d time.Duration) string {
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
	default:
		return fmt.Sprintf("%dh%02dm", int(d.Hours()), int(d.Minutes())%60)
	}
}

// jobStrip is the rendered strip, "" when nothing runs.
func (m *model) jobStrip(cfg *uiCfg) string {
	rows := m.jobRows(cfg)
	if len(rows) == 0 {
		return ""
	}
	return strings.Join(rows, "\n")
}
