package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// apiFixture is a live httptest server over a supervisor whose history
// directory is hand-written under a t.TempDir() HOME. No child is
// spawned unless a test asks for one.
type apiFixture struct {
	*fixture
	srv *httptest.Server
}

func newAPI(t *testing.T, extraEnv ...string) *apiFixture {
	t.Helper()
	f := newFixture(t, extraEnv...)
	srv := httptest.NewServer(NewAPI(f.sup))
	t.Cleanup(srv.Close)
	return &apiFixture{fixture: f, srv: srv}
}

func (f *apiFixture) do(t *testing.T, method, path, body string) (int, map[string]any) {
	t.Helper()
	var rdr io.Reader
	if body != "" {
		rdr = strings.NewReader(body)
	}
	req, err := http.NewRequest(method, f.srv.URL+path, rdr)
	if err != nil {
		t.Fatal(err)
	}
	resp, err := f.srv.Client().Do(req)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer resp.Body.Close()
	var out map[string]any
	raw, _ := io.ReadAll(resp.Body)
	if len(raw) > 0 && json.Unmarshal(raw, &out) != nil {
		// The mux writes its own plain-text 405; everything the API
		// itself answers must be JSON.
		if resp.StatusCode != http.StatusMethodNotAllowed {
			t.Fatalf("%s %s: body %q is not JSON", method, path, raw)
		}
	}
	return resp.StatusCode, out
}

// rowOf pulls the "session" object out of a reply.
func rowOf(t *testing.T, body map[string]any) map[string]any {
	t.Helper()
	row, ok := body["session"].(map[string]any)
	if !ok {
		t.Fatalf("no session in %v", body)
	}
	return row
}

func sessionIDs(t *testing.T, body map[string]any) []string {
	t.Helper()
	list, ok := body["sessions"].([]any)
	if !ok {
		t.Fatalf("no sessions in %v", body)
	}
	var out []string
	for _, s := range list {
		out = append(out, s.(map[string]any)["id"].(string))
	}
	return out
}

func TestAPIHealth(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	code, body := f.do(t, "GET", "/api/health", "")
	if code != http.StatusOK || body["ok"] != true {
		t.Fatalf("health = %d %v", code, body)
	}
	if code, _ := f.do(t, "POST", "/api/health", ""); code != http.StatusMethodNotAllowed {
		t.Errorf("POST /api/health = %d, want 405", code)
	}
}

func TestAPIListSessions(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	f.seed(t, "old",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/one", "repo": "/one", "branch": "main"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "first prompt"}},
		history.Entry{Seq: 3, At: now, Kind: "done", Data: nil},
	)
	f.seed(t, "new",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/two"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "second"}},
		history.Entry{Seq: 3, At: now, Kind: "cancelled", Data: nil},
	)
	// List orders by mtime, so make the order deterministic.
	old := now.Add(-time.Hour)
	if err := os.Chtimes(filepath.Join(f.hist, "old.jsonl"), old, old); err != nil {
		t.Fatal(err)
	}

	code, body := f.do(t, "GET", "/api/sessions", "")
	if code != http.StatusOK {
		t.Fatalf("list = %d %v", code, body)
	}
	if ids := sessionIDs(t, body); len(ids) != 2 || ids[0] != "new" {
		t.Fatalf("ids = %v, want [new old]", ids)
	}
	rows := body["sessions"].([]any)
	first := rows[0].(map[string]any)
	if first["status"] != string(StatusStopped) {
		t.Errorf("new status = %v, want stopped", first["status"])
	}
	if first["live"] != false || first["archived"] != false {
		t.Errorf("new row = %v", first)
	}
	second := rows[1].(map[string]any)
	if second["status"] != string(StatusDone) || second["title"] != "first prompt" {
		t.Errorf("old row = %v", second)
	}
	if second["repo"] != "/one" || second["branch"] != "main" || second["cwd"] != "/one" {
		t.Errorf("old row lost its meta: %v", second)
	}
	if second["entries"].(float64) != 3 {
		t.Errorf("old entries = %v, want 3", second["entries"])
	}

	// cwd filters to sessions recorded in that directory.
	_, body = f.do(t, "GET", "/api/sessions?cwd=/two", "")
	if ids := sessionIDs(t, body); len(ids) != 1 || ids[0] != "new" {
		t.Errorf("cwd filter = %v", ids)
	}
	_, body = f.do(t, "GET", "/api/sessions?cwd=/nowhere", "")
	if ids := sessionIDs(t, body); len(ids) != 0 {
		t.Errorf("cwd filter on an unused dir = %v", ids)
	}
}

func TestAPIGetSession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	f.seed(t, "s1",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/w"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hello"}},
		history.Entry{Seq: 3, At: now, Kind: "assistant", Data: map[string]any{"text": "hi"}},
		history.Entry{Seq: 4, At: now, Kind: "done", Data: nil},
	)

	code, body := f.do(t, "GET", "/api/sessions/s1", "")
	if code != http.StatusOK {
		t.Fatalf("get = %d %v", code, body)
	}
	if rowOf(t, body)["status"] != string(StatusDone) {
		t.Errorf("status = %v", rowOf(t, body)["status"])
	}
	lines := body["entries"].([]any)
	if len(lines) != 4 {
		t.Fatalf("entries = %d, want 4", len(lines))
	}
	if lines[1].(map[string]any)["text"] != "hello" {
		t.Errorf("entry 1 = %v", lines[1])
	}

	_, body = f.do(t, "GET", "/api/sessions/s1?since=2", "")
	if lines := body["entries"].([]any); len(lines) != 2 {
		t.Errorf("since=2 returned %d entries, want 2", len(lines))
	}
	_, body = f.do(t, "GET", "/api/sessions/s1?limit=1", "")
	if lines := body["entries"].([]any); len(lines) != 1 {
		t.Errorf("limit=1 returned %d entries, want 1", len(lines))
	}
	// An empty slice, never a null: clients iterate it.
	_, body = f.do(t, "GET", "/api/sessions/s1?since=99", "")
	if lines, ok := body["entries"].([]any); !ok || len(lines) != 0 {
		t.Errorf("since past the end = %v, want []", body["entries"])
	}
}

func TestAPIErrorPaths(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s1")

	cases := []struct {
		name, method, path, body string
		want                     int
	}{
		{"unknown session", "GET", "/api/sessions/nope", "", http.StatusNotFound},
		{"unknown session prompt", "POST", "/api/sessions/nope/prompt", `{"text":"x"}`, http.StatusNotFound},
		{"unknown session rename", "POST", "/api/sessions/nope/rename", `{"title":"x"}`, http.StatusNotFound},
		{"unknown session archive", "POST", "/api/sessions/nope/archive", "", http.StatusNotFound},
		{"unknown session interrupt", "POST", "/api/sessions/nope/interrupt", "", http.StatusNotFound},
		{"bad since", "GET", "/api/sessions/s1?since=abc", "", http.StatusBadRequest},
		{"bad limit", "GET", "/api/sessions/s1?limit=x", "", http.StatusBadRequest},
		{"bad body", "POST", "/api/sessions/s1/rename", "{not json", http.StatusBadRequest},
		{"missing cwd", "POST", "/api/sessions", `{"prompt":"hi"}`, http.StatusBadRequest},
		{"cwd is not a dir", "POST", "/api/sessions", `{"cwd":"/definitely/not/here"}`, http.StatusBadRequest},
		{"wrong method", "GET", "/api/sessions/s1/prompt", "", http.StatusMethodNotAllowed},
		{"interrupt with no child", "POST", "/api/sessions/s1/interrupt", "", http.StatusNotFound},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			code, body := f.do(t, tc.method, tc.path, tc.body)
			if code != tc.want {
				t.Fatalf("%s %s = %d %v, want %d", tc.method, tc.path, code, body, tc.want)
			}
			if code == http.StatusMethodNotAllowed {
				return // the mux's own 405 has no JSON body
			}
			msg, _ := body["error"].(string)
			if !strings.HasPrefix(msg, "serve:") {
				t.Errorf("error = %q, want a serve:-prefixed message", msg)
			}
		})
	}
}

func TestAPIRenameArchiveUnarchive(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s1",
		history.Entry{Seq: 1, At: time.Now(), Kind: "input", Data: map[string]any{"text": "from history"}},
		history.Entry{Seq: 2, At: time.Now(), Kind: "done", Data: nil},
	)

	code, body := f.do(t, "POST", "/api/sessions/s1/rename", `{"title":"my session"}`)
	if code != http.StatusOK || rowOf(t, body)["title"] != "my session" {
		t.Fatalf("rename = %d %v", code, body)
	}
	// The rename lives beside history, which is never rewritten.
	es, err := f.sup.Entries("s1")
	if err != nil || len(es) != 2 {
		t.Fatalf("history changed under the rename: %v %v", es, err)
	}
	// Clearing the title falls back to the history title.
	_, body = f.do(t, "POST", "/api/sessions/s1/rename", `{"title":""}`)
	if got := rowOf(t, body)["title"]; got != "from history" {
		t.Errorf("cleared title = %v, want the history title", got)
	}

	code, body = f.do(t, "POST", "/api/sessions/s1/archive", "")
	if code != http.StatusOK {
		t.Fatalf("archive = %d %v", code, body)
	}
	row := rowOf(t, body)
	if row["archived"] != true || row["live"] != false {
		t.Fatalf("archived row = %v", row)
	}
	_, body = f.do(t, "GET", "/api/sessions", "")
	if ids := sessionIDs(t, body); len(ids) != 0 {
		t.Errorf("archived session still listed: %v", ids)
	}
	_, body = f.do(t, "GET", "/api/sessions?all=1", "")
	if ids := sessionIDs(t, body); len(ids) != 1 {
		t.Errorf("all=1 = %v, want the archived session", ids)
	}
	// Archiving is reversible.
	code, body = f.do(t, "POST", "/api/sessions/s1/unarchive", "")
	if code != http.StatusOK || rowOf(t, body)["archived"] != false {
		t.Fatalf("unarchive = %d %v", code, body)
	}
	_, body = f.do(t, "GET", "/api/sessions", "")
	if ids := sessionIDs(t, body); len(ids) != 1 {
		t.Errorf("unarchived session missing from the list: %v", ids)
	}
}

func TestAPIPromptOnArchivedIsConflict(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s1")
	if code, body := f.do(t, "POST", "/api/sessions/s1/archive", ""); code != http.StatusOK {
		t.Fatalf("archive = %d %v", code, body)
	}
	code, body := f.do(t, "POST", "/api/sessions/s1/prompt", `{"text":"hi"}`)
	if code != http.StatusConflict {
		t.Fatalf("prompt on an archived session = %d %v, want 409", code, body)
	}
}

func TestAPIAnswerWithNoAskIsConflict(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s1")
	code, body := f.do(t, "POST", "/api/sessions/s1/answer", `{"text":"a"}`)
	if code != http.StatusConflict {
		t.Fatalf("answer with nothing armed = %d %v, want 409", code, body)
	}
	if msg, _ := body["error"].(string); !strings.Contains(msg, "no pending ask") {
		t.Errorf("error = %q", msg)
	}
}

func TestAPICreateAndPromptAndAsk(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envNewID+"=created", envAsk+"=1")

	code, body := f.do(t, "POST", "/api/sessions", `{"cwd":"`+f.home+`","prompt":"hello"}`)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	row := rowOf(t, body)
	if row["id"] != "created" || row["live"] != true {
		t.Fatalf("created row = %v", row)
	}

	waitFor(t, "the ask to arm", func() bool { return f.sup.PendingAsk("created") != nil })
	// A prompt now would be eaten as the answer.
	if code, _ := f.do(t, "POST", "/api/sessions/created/prompt", `{"text":"x"}`); code != http.StatusConflict {
		t.Errorf("prompt with an armed ask = %d, want 409", code)
	}
	if code, body := f.do(t, "POST", "/api/sessions/created/answer", `{"text":"a"}`); code != http.StatusOK {
		t.Fatalf("answer = %d %v", code, body)
	}
	waitFor(t, "the answer to land", func() bool {
		for _, e := range f.sup.Recent("created") {
			if e.Text == "answered:a" {
				return true
			}
		}
		return false
	})
	// With the ask cleared, prompting works again and reuses the child.
	if code, body := f.do(t, "POST", "/api/sessions/created/prompt", `{"text":"more"}`); code != http.StatusOK {
		t.Fatalf("prompt = %d %v", code, body)
	}
	if n := f.startCount(t); n != 1 {
		t.Errorf("started %d children, want 1", n)
	}
	if code, body := f.do(t, "POST", "/api/sessions/created/interrupt", ""); code != http.StatusOK {
		t.Fatalf("interrupt = %d %v", code, body)
	}
}

// readFrames reads SSE frames (blank-line separated) until n are in
// hand or the context deadline fires.
func readFrames(t *testing.T, body io.Reader, n int) []string {
	t.Helper()
	sc := bufio.NewScanner(body)
	var frames []string
	var cur []string
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			if len(cur) > 0 {
				frames = append(frames, strings.Join(cur, "\n"))
				cur = nil
				if len(frames) >= n {
					return frames
				}
			}
			continue
		}
		cur = append(cur, line)
	}
	return frames
}

func TestAPIEventsStream(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s1")

	// One event already in the ring, so the replay path is exercised.
	fake := &child{id: "s1", done: make(chan struct{})}
	f.sup.emit(fake, "assistant", "replayed", nil)

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, "GET", f.srv.URL+"/api/sessions/s1/events", nil)
	if err != nil {
		t.Fatal(err)
	}
	resp, err := f.srv.Client().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if ct := resp.Header.Get("Content-Type"); ct != "text/event-stream" {
		t.Fatalf("Content-Type = %q", ct)
	}
	if cc := resp.Header.Get("Cache-Control"); cc != "no-cache" {
		t.Errorf("Cache-Control = %q", cc)
	}

	// Watching a session must not start one.
	if f.sup.Live("s1") {
		t.Error("the events stream spawned a child")
	}

	// A live event after the subscription; the handler subscribes
	// before replaying, so both must arrive.
	go func() {
		// No t.Fatal off the test goroutine: if the subscriber never
		// registers, the frame never arrives and the read below fails.
		for range 1000 {
			f.sup.mu.Lock()
			n := len(f.sup.subs["s1"])
			f.sup.mu.Unlock()
			if n > 0 {
				break
			}
			time.Sleep(5 * time.Millisecond)
		}
		f.sup.emit(fake, "done", "finished", map[string]any{"turn": 1})
	}()

	frames := readFrames(t, resp.Body, 3)
	if len(frames) < 3 {
		t.Fatalf("got %d frames: %v", len(frames), frames)
	}
	if frames[0] != ": open" {
		t.Errorf("first frame = %q, want the open comment", frames[0])
	}
	if !strings.Contains(frames[1], `"kind":"assistant"`) || !strings.Contains(frames[1], "id: 1") {
		t.Errorf("replayed frame = %q", frames[1])
	}
	if !strings.Contains(frames[2], `"kind":"done"`) {
		t.Errorf("live frame = %q", frames[2])
	}
	var ev Event
	data := frames[2][strings.Index(frames[2], "data: ")+len("data: "):]
	if err := json.Unmarshal([]byte(data), &ev); err != nil {
		t.Fatalf("frame data %q: %v", data, err)
	}
	if ev.Session != "s1" || ev.Kind != "done" || ev.Text != "finished" || ev.Extra["turn"] != float64(1) {
		t.Errorf("event = %+v", ev)
	}
	if ev.At.IsZero() {
		t.Error("event has no timestamp")
	}

	// Disconnecting must unsubscribe and leave the supervisor working.
	cancel()
	resp.Body.Close()
	waitFor(t, "the subscriber to be dropped", func() bool {
		f.sup.mu.Lock()
		defer f.sup.mu.Unlock()
		return len(f.sup.subs["s1"]) == 0
	})
	f.sup.emit(fake, "assistant", "after the hangup", nil)
	if n := len(f.sup.Recent("s1")); n != 3 {
		t.Fatalf("ring = %d events, want 3 — the supervisor wedged on a gone subscriber", n)
	}
	if code, body := f.do(t, "GET", "/api/sessions/s1", ""); code != http.StatusOK {
		t.Fatalf("the API is still serving? %d %v", code, body)
	}
}

func TestAPIEventsUnknownSession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	if code, _ := f.do(t, "GET", "/api/sessions/nope/events", ""); code != http.StatusNotFound {
		t.Errorf("events on an unknown session = %d, want 404", code)
	}
}
