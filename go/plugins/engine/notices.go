package engine

import (
	"cmp"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// storedNoticePoll is how often a running session looks for notices
// another process appended to its file: a report is not urgent to the
// second, and a stat per second is free.
const storedNoticePoll = time.Second

// pollStoredNotices is the loop's stored-notice delivery (loop.go
// deliverStoredNotices), copied rather than shared because the loop's
// copy stays with the loop. serve appends a "notice" to the file of a
// parent it does not run (resumed in the TUI, a plain `bough -r`); a
// mount-time read alone would leave that live session deaf until a
// remount.
func pollStoredNotices(h loop.History, notify func() func(string), stop <-chan struct{}) {
	seen := fileSize(h)
	deliverStoredNotices(h, notify)
	tick := time.NewTicker(storedNoticePoll)
	defer tick.Stop()
	for {
		select {
		case <-stop:
			return
		case <-tick.C:
			if n := fileSize(h); n != seen {
				seen = n
				deliverStoredNotices(h, notify)
			}
		}
	}
}

func fileSize(h loop.History) int64 {
	if h == nil || h.Path() == "" {
		return 0
	}
	st, err := os.Stat(h.Path())
	if err != nil {
		return 0
	}
	return st.Size()
}

// deliverStoredNotices queues each undelivered notice addressed to this
// file exactly once. It reads the raw file, all branches, because a
// notice appended while another process held the file sits on a side
// branch; only notices addressed here count, so a fork does not
// re-deliver its parent's. Each is marked delivered BEFORE it is
// queued: a crash between the two loses the notice rather than
// delivering it twice.
func deliverStoredNotices(h loop.History, notify func() func(string)) {
	n := notify()
	if n == nil || h == nil || h.Path() == "" {
		return // marking without a queue would lose the notice
	}
	entries, err := history.ReadFile(h.Path())
	if err != nil {
		return
	}
	own := strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	done := map[string]bool{}
	for _, e := range entries {
		if e.Kind == "notice-delivered" {
			id, _ := e.Data["id"].(string)
			done[id] = true
		}
	}
	var todo []history.Entry
	for _, e := range entries {
		id, _ := e.Data["id"].(string)
		if e.Kind != "notice" || id == "" || done[id] || e.Data["to"] != own {
			continue
		}
		done[id] = true
		todo = append(todo, e)
	}
	slices.SortFunc(todo, func(a, b history.Entry) int { return cmp.Compare(a.Seq, b.Seq) })
	for _, e := range todo {
		h.Append("notice-delivered", map[string]any{"id": e.Data["id"]})
		text, _ := e.Data["text"].(string)
		n(text)
	}
}
