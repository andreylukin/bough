package ui

import (
	"bytes"
	"strings"
	"sync"
	"testing"
	"time"
)

type recordAnswers struct{ got []string }

func (r *recordAnswers) Answer(id, text string) error {
	r.got = append(r.got, id+"="+text)
	return nil
}

// Not parallel: the headless pump state is process-wide, like the other
// tests in this package that swap hlOut/hlSteer.

// A notice line with an ask pending goes to job-notices and leaves the
// ask armed for the next real line; it owes no done.
func TestHeadlessNoticeDoesNotAnswerAsk(t *testing.T) {
	var got []string
	ans := &recordAnswers{}
	hlMu.Lock()
	oldNotify, oldAsk, oldAnswer := hlNotify, hlAsk, hlAnswer
	hlNotify = func(s string) bool { got = append(got, s); return true }
	hlAsk = &hlAskState{id: "q1", options: []string{"a", "b"}}
	hlAnswer = ans
	hlMu.Unlock()
	defer func() {
		hlMu.Lock()
		hlNotify, hlAsk, hlAnswer = oldNotify, oldAsk, oldAnswer
		hlMu.Unlock()
	}()
	before := hlPending.Load()

	hlLineIn(`{"notice": "agent x finished"}`)
	if len(got) != 1 || got[0] != "agent x finished" {
		t.Fatalf("notify got %q", got)
	}
	if len(ans.got) != 0 {
		t.Fatalf("notice answered the ask: %q", ans.got)
	}
	if hlPending.Load() != before {
		t.Fatalf("notice changed hlPending %d -> %d", before, hlPending.Load())
	}
	hlLineIn("2")
	if len(ans.got) != 1 || ans.got[0] != "q1=b" {
		t.Fatalf("ask answers = %q, want q1=b", ans.got)
	}
}

// Without the service, past the wait, the notice is dropped with an
// error line.
func TestHeadlessNoticeWithoutService(t *testing.T) {
	var errb bytes.Buffer
	oldErr, oldWait := hlErr, hlNoticeWait
	hlErr, hlNoticeWait = &errb, 100*time.Millisecond
	hlMu.Lock()
	oldNotify := hlNotify
	hlNotify = nil
	hlMu.Unlock()
	defer func() {
		hlErr, hlNoticeWait = oldErr, oldWait
		hlMu.Lock()
		hlNotify = oldNotify
		hlMu.Unlock()
	}()
	hlLineIn(`{"notice": "x"}`)
	if !strings.Contains(errb.String(), "notice dropped: no job-notices service") {
		t.Fatalf("stderr = %q", errb.String())
	}
}

// A notice arriving in a reload gap (no hook yet, then job-notices not
// mounted) waits for the remount instead of being dropped.
func TestHeadlessNoticeWaitsOutReload(t *testing.T) {
	var errb bytes.Buffer
	oldErr := hlErr
	hlErr = &errb
	hlMu.Lock()
	oldNotify := hlNotify
	hlNotify = nil
	hlMu.Unlock()
	defer func() {
		hlErr = oldErr
		hlMu.Lock()
		hlNotify = oldNotify
		hlMu.Unlock()
	}()
	var mu sync.Mutex
	var got []string
	mounted := false
	go func() {
		time.Sleep(100 * time.Millisecond)
		hlMu.Lock()
		// Remounted ui row whose tools row is not up for one more beat.
		hlNotify = func(s string) bool {
			mu.Lock()
			defer mu.Unlock()
			if !mounted {
				mounted = true
				return false
			}
			got = append(got, s)
			return true
		}
		hlMu.Unlock()
	}()
	hlLineIn(`{"notice": "agent y finished"}`)
	mu.Lock()
	defer mu.Unlock()
	if len(got) != 1 || got[0] != "agent y finished" || errb.Len() != 0 {
		t.Fatalf("got %q, stderr %q", got, errb.String())
	}
}
