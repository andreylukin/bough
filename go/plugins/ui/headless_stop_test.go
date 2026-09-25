package ui

import (
	"bytes"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

type recordHist struct{ kinds []string }

func (r *recordHist) Append(kind string, data map[string]any) history.Entry {
	r.kinds = append(r.kinds, kind)
	return history.Entry{Kind: kind, Data: data}
}

// Not parallel: the headless pump state is process-wide.

// A stdin line read after the process was interrupted is not run: serve
// writes a first prompt while the child is still mounting, and a Stop in
// that gap SIGINTs a child whose pump has not started. The launcher then
// finished mounting, the pump read the "!" or "/" line and ran it (a
// shell command the person had just stopped), and only then exited.
func TestHeadlessLineAfterStopIsDropped(t *testing.T) {
	h := &recordHist{}
	var out bytes.Buffer
	oldOut := hlOut
	hlOut = &out
	hlMu.Lock()
	oldHist := hlHist
	hlHist = h
	hlMu.Unlock()
	defer func() {
		hlOut = oldOut
		hlMu.Lock()
		hlHist = oldHist
		hlMu.Unlock()
		hlStopped.Store(false)
	}()

	StopInput()
	hlLineIn("!echo ran")
	if len(h.kinds) != 0 || out.Len() != 0 {
		t.Fatalf("a line read after the stop ran: history %v, output %q", h.kinds, out.String())
	}

	hlStopped.Store(false)
	hlLineIn("!echo ran")
	if len(h.kinds) == 0 {
		t.Fatal("a line with no stop did not run: the test is not checking the drop")
	}
}
