package ui

import (
	"bytes"
	"encoding/json"
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

// withAsk arms a pending ask for one test and records what reaches
// job-notices and the Asker.
func withAsk(t *testing.T, ask *hlAskState) (notices *[]string, ans *recordAnswers) {
	var got []string
	ans = &recordAnswers{}
	hlMu.Lock()
	oldNotify, oldAsk, oldAnswer, oldTyped := hlNotify, hlAsk, hlAnswer, hlTyped
	hlNotify = func(s string) bool { got = append(got, s); return true }
	hlAsk, hlAnswer, hlTyped = ask, ans, true
	hlMu.Unlock()
	t.Cleanup(func() {
		hlMu.Lock()
		hlNotify, hlAsk, hlAnswer, hlTyped = oldNotify, oldAsk, oldAnswer, oldTyped
		hlMu.Unlock()
	})
	return &got, ans
}

// A background agent's report arriving while tools.secret is open is
// still a report: it was stored as the secret and never delivered.
// Found by tests/model/mbt/notice_during_open_ask_test.go (AskSecret,
// Notify).
func TestHeadlessNoticeDoesNotAnswerSecret(t *testing.T) {
	notices, ans := withAsk(t, &hlAskState{id: "s1", secret: true})
	hlLineIn(`{"notice": "agent x finished"}`)
	if len(*notices) != 1 || (*notices)[0] != "agent x finished" {
		t.Fatalf("notify got %q", *notices)
	}
	if len(ans.got) != 0 {
		t.Fatalf("notice answered the secret: %q", ans.got)
	}
	hlLineIn(`{"answer": "hunter2", "ask": "s1"}`)
	if len(ans.got) != 1 || ans.got[0] != "s1=hunter2" {
		t.Fatalf("secret answers = %q, want s1=hunter2", ans.got)
	}
}

// serve's {"answer", "ask"} line is the answer whatever its text looks like: a
// reply that parses as {"notice"} or {"prompt"} is not a report and not
// a prompt. Found by tests/model/mbt/notice_during_open_ask_test.go
// (AskPlain, AnswerNoticeLike).
func TestHeadlessAnswerLineIsTheAnswer(t *testing.T) {
	for _, text := range []string{`{"notice":"x"}`, `{"prompt":"y"}`, "1"} {
		notices, ans := withAsk(t, &hlAskState{id: "q1", options: []string{"a", "b"}})
		b, _ := json.Marshal(map[string]string{"answer": text, "ask": "q1"})
		hlLineIn(string(b))
		want := "q1=" + text
		if text == "1" {
			want = "q1=a" // an option's number still picks it
		}
		if len(*notices) != 0 || len(ans.got) != 1 || ans.got[0] != want {
			t.Fatalf("answer %q: notices %q, answers %q, want %s", text, *notices, ans.got, want)
		}
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
