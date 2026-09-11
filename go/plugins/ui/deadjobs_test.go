package ui

import (
	"slices"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// A started job with no finished notice on file died with its process;
// a "matched while running" notice does not count as finished.
func TestDeadJobs(t *testing.T) {
	e := func(kind, text string) history.Entry {
		return history.Entry{Kind: kind, Data: map[string]any{"text": text}}
	}
	entries := []history.Entry{
		e("result", "job 1 started in the background (limit 2m0s): sleep 3"),
		e("result", "job 2 started in the background (limit 2m0s): cat feed"),
		e("job", "job 2 matched \"x\" while running: cat feed"),
		e("input", "[background job] A command...\n\njob 1 [exited 0] sleep 3 (3s)"),
	}
	if got := deadJobs(entries); !slices.Equal(got, []int{2}) {
		t.Fatalf("deadJobs = %v, want [2]", got)
	}
}
