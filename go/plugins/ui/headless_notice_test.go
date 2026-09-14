package ui

import (
	"bytes"
	"strings"
	"testing"
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
	hlNotify = func(s string) { got = append(got, s) }
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

// Without the service the notice is dropped with an error line.
func TestHeadlessNoticeWithoutService(t *testing.T) {
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
	hlLineIn(`{"notice": "x"}`)
	if !strings.Contains(errb.String(), "notice dropped: no job-notices service") {
		t.Fatalf("stderr = %q", errb.String())
	}
}
