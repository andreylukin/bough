package serve

import (
	"net/http"
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

func inputsWith(t *testing.T, f *apiFixture, id, text string) int {
	t.Helper()
	es, err := history.Read(filepath.Join(f.hist, id+".jsonl"))
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range es {
		if e.Kind == "input" && e.Data["text"] == text {
			n++
		}
	}
	return n
}

// A retry under the same request id is written once, and serve can say
// it has the id, also after a restart (the page's question once its
// answer was lost: tests/model/specs/prompt_idempotency_reload.fizz).
func TestAPIPromptRequestIDWritesOnce(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	f.seed(t, "s1")
	if code, body := f.do(t, "GET", "/api/sessions/s1/prompts/r1", ""); code != http.StatusOK || body["state"] != ReqNone {
		t.Fatalf("state before any send = %d %v, want none", code, body)
	}
	for range 2 {
		if code, body := f.do(t, "POST", "/api/sessions/s1/prompt", `{"text":"hello","id":"r1"}`); code != http.StatusOK {
			t.Fatalf("prompt = %d %v", code, body)
		}
	}
	// Lines are read in order: once "after" is in, a second "hello"
	// would be too.
	if code, body := f.do(t, "POST", "/api/sessions/s1/prompt", `{"text":"after"}`); code != http.StatusOK {
		t.Fatalf("prompt = %d %v", code, body)
	}
	waitFor(t, "the line after", func() bool { return inputsWith(t, f, "s1", "after") == 1 })
	if n := inputsWith(t, f, "s1", "hello"); n != 1 {
		t.Fatalf("hello recorded %d times under one request id, want 1", n)
	}
	if _, body := f.do(t, "GET", "/api/sessions/s1/prompts/r1", ""); body["state"] != ReqLanded {
		t.Errorf("state after it landed = %v, want landed", body)
	}

	// A restarted serve still knows the id: the retry writes nothing.
	f.sup.Close()
	sup, err := NewSupervisor(f.sup.opt)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	if st := sup.PromptState("s1", "r1"); st != ReqLanded {
		t.Fatalf("state after a restart = %q, want landed", st)
	}
	if err := sup.SendOnce("s1", "r1", "hello"); err != nil {
		t.Fatal(err)
	}
	if err := sup.Send("s1", "after2"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the line after the restart", func() bool { return inputsWith(t, f, "s1", "after2") == 1 })
	if n := inputsWith(t, f, "s1", "hello"); n != 1 {
		t.Fatalf("hello recorded %d times after a retry across a restart, want 1", n)
	}
}

// A line whose child died before recording it is gone: serve says none,
// and the same id writes it again. The fake child here records nothing
// in history, so every line it was given stays unread.
func TestPromptStateLostWithItsChild(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "s1")
	if err := f.sup.SendOnce("s1", "r2", "second"); err != nil {
		t.Fatal(err)
	}
	if st := f.sup.PromptState("s1", "r2"); st != ReqUnread {
		t.Fatalf("state of a line the child has not recorded = %q, want unread", st)
	}
	if err := f.sup.SendOnce("s1", "r2", "second"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the child", func() bool { return f.startCount(t) == 1 })
	if err := f.sup.Kill("s1"); err != nil {
		t.Fatal(err)
	}
	if st := f.sup.PromptState("s1", "r2"); st != ReqNone {
		t.Fatalf("state of a line whose child died unread = %q, want none", st)
	}
	// The same id is written again, to a new child.
	if err := f.sup.SendOnce("s1", "r2", "second"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a second child", func() bool { return f.startCount(t) == 2 })
	if st := f.sup.PromptState("s1", "r2"); st != ReqUnread {
		t.Fatalf("state of the rewritten line = %q, want unread", st)
	}
}

// A retried create under one request id makes one session.
func TestAPICreateRequestIDMakesOne(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envNewID+"=created")
	body := `{"cwd":"` + f.home + `","requestId":"c1"}`
	code, first := f.do(t, "POST", "/api/sessions", body)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, first)
	}
	code, again := f.do(t, "POST", "/api/sessions", body)
	if code != http.StatusOK {
		t.Fatalf("retried create = %d %v, want 200", code, again)
	}
	if a, b := rowOf(t, first)["id"], rowOf(t, again)["id"]; a != b {
		t.Fatalf("retried create made %v, first made %v", b, a)
	}
}
