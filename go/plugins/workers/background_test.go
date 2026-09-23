package workers

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/history"
)

// pathHist is a history service with a file path and entries, the two
// seams a background spawn reads.
type pathHist struct {
	memHist
	path string
	meta map[string]any
}

func (p *pathHist) Path() string { return p.path }
func (p *pathHist) Entries() []history.Entry {
	return []history.Entry{{Seq: 1, Kind: "meta", Data: p.meta}}
}

// fakeServe stands in for bough serve: it records requests and answers
// with a scripted status and body per path.
type fakeServe struct {
	mu     sync.Mutex
	bodies map[string]string // path -> last request body
	codes  map[string]int
	reply  map[string]string
}

func (f *fakeServe) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b, _ := io.ReadAll(r.Body)
	f.mu.Lock()
	defer f.mu.Unlock()
	f.bodies[r.URL.Path] = string(b) + r.URL.RawQuery
	w.Header().Set("Content-Type", "application/json")
	code := f.codes[r.URL.Path]
	if code == 0 {
		code = 200
	}
	w.WriteHeader(code)
	io.WriteString(w, f.reply[r.URL.Path])
}

func (f *fakeServe) body(path string) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.bodies[path]
}

type bgRig struct {
	cm    *codemode.CodeMode
	serve *fakeServe
}

// bgMount mounts workers with a HOME whose serve.pid names this process
// and an httptest serve (withServe), or no pidfile at all.
func bgMount(t *testing.T, withServe bool, provide func(*kernel.Context), meta map[string]any) *bgRig {
	t.Helper()
	home := t.TempDir()
	fs := &fakeServe{bodies: map[string]string{}, codes: map[string]int{}, reply: map[string]string{}}
	if withServe {
		srv := httptest.NewServer(fs)
		t.Cleanup(srv.Close)
		os.MkdirAll(filepath.Join(home, ".bough"), 0o755)
		addr := strings.TrimPrefix(srv.URL, "http://")
		if err := os.WriteFile(filepath.Join(home, ".bough", "serve.pid"), fmt.Appendf(nil, "%d %s\t%s\t\t\n", os.Getpid(), addr, home), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	kctx := kernel.NewContext()
	cm := codemode.New(5 * time.Second)
	kctx.Provide("llm", &scriptLLM{script: []string{"unused"}})
	kctx.Provide("codemode", cm)
	kctx.Provide("history", &pathHist{path: filepath.Join(home, "sess-parent.jsonl"), meta: meta})
	if provide != nil {
		provide(kctx)
	}
	if err := apply(kctx, nil, home); err != nil {
		t.Fatal(err)
	}
	return &bgRig{cm: cm, serve: fs}
}

// sameJSON compares two JSON objects ignoring key order (goja does not
// keep a Go map's keys in any order).
func sameJSON(a, b string) bool {
	var x, y map[string]any
	if json.Unmarshal([]byte(a), &x) != nil || json.Unmarshal([]byte(b), &y) != nil {
		return false
	}
	xb, _ := json.Marshal(x)
	yb, _ := json.Marshal(y)
	return string(xb) == string(yb)
}

func (r *bgRig) run(t *testing.T, js string) string {
	t.Helper()
	out, err := r.cm.Run(`try { JSON.stringify(` + js + `) } catch (e) { "THREW " + e.message }`)
	if err != nil {
		t.Fatalf("%s: %v", js, err)
	}
	return strings.TrimSpace(out)
}

func TestBackgroundSpawnReturnsAtOnce(t *testing.T) {
	t.Parallel()
	r := bgMount(t, true, nil, nil)
	r.serve.codes["/api/sessions"] = 201
	r.serve.reply["/api/sessions"] = `{"session":{"id":"child-1"},"queued":false}`
	if got := r.run(t, `tools.spawn("list go files", {background: true})`); !sameJSON(got, `{"session":"child-1","status":"running"}`) {
		t.Fatalf("spawn = %s", got)
	}
	var req map[string]any
	json.Unmarshal([]byte(r.serve.body("/api/sessions")), &req)
	if req["spawnedBy"] != "sess-parent" || req["prompt"] != "list go files" || req["maxPerSession"] != float64(200) || req["maxRunning"] != float64(16) || req["slug"] != nil {
		t.Fatalf("request = %v", req)
	}

	r.serve.reply["/api/sessions"] = `{"session":{"id":"child-2"},"queued":true}`
	if got := r.run(t, `tools.spawn("t", {background: true, project: "demo"})`); !sameJSON(got, `{"session":"child-2","status":"queued"}`) {
		t.Fatalf("queued spawn = %s", got)
	}
	json.Unmarshal([]byte(r.serve.body("/api/sessions")), &req)
	if req["slug"] != "demo" {
		t.Fatalf("project not forwarded as slug: %v", req)
	}
}

func TestBackgroundSpawnErrors(t *testing.T) {
	t.Parallel()
	noServe := bgMount(t, false, nil, nil)
	if got := noServe.run(t, `tools.spawn("t", {background: true})`); got != "THREW background agents need bough serve (start it with `bough serve`)" {
		t.Fatalf("no serve = %s", got)
	}

	fresh := bgMount(t, true, func(k *kernel.Context) { k.Provide("session-spawned-by", "p0") }, nil)
	if got := fresh.run(t, `tools.spawn("t", {background: true})`); got != "THREW workers: a background agent cannot start agents (depth 1)" {
		t.Fatalf("env depth = %s", got)
	}
	resumed := bgMount(t, true, nil, map[string]any{"spawned_by": "p0"})
	if got := resumed.run(t, `tools.spawn("t", {background: true})`); got != "THREW workers: a background agent cannot start agents (depth 1)" {
		t.Fatalf("meta depth = %s", got)
	}

	// A thread a person started from the project page has main as its
	// parent, fresh or resumed, and still delegates: serve decides.
	thread := bgMount(t, true, func(k *kernel.Context) { k.Provide("session-spawned-by", "p0"); k.Provide("session-thread", true) }, map[string]any{"spawned_by": "p0"})
	thread.serve.codes["/api/sessions"] = 201
	thread.serve.reply["/api/sessions"] = `{"session":{"id":"c1"},"queued":false}`
	if got := thread.run(t, `tools.spawn("t", {background: true})`); !sameJSON(got, `{"session":"c1","status":"running"}`) {
		t.Fatalf("thread spawn = %s", got)
	}
	thread.serve.codes["/api/sessions"] = 409
	thread.serve.reply["/api/sessions"] = `{"error":"depth"}`
	if got := thread.run(t, `tools.spawn("t", {background: true})`); got != "THREW workers: a background agent cannot start agents (depth 1)" {
		t.Fatalf("serve depth = %s", got)
	}

	r := bgMount(t, true, nil, nil)
	if got := r.run(t, `tools.spawn("t", {background: true, type: "object"})`); got != "THREW workers: a background agent reports text; drop the schema" {
		t.Fatalf("schema = %s", got)
	}
	r.serve.codes["/api/sessions"] = 429
	r.serve.reply["/api/sessions"] = `{"error":"serve: api: background agent limit reached (200 per session)"}`
	if got := r.run(t, `tools.spawn("t", {background: true})`); got != "THREW workers: background agent limit reached (200 per session) — do the remaining work yourself" {
		t.Fatalf("limit = %s", got)
	}
	r.serve.codes["/api/sessions"] = 400
	r.serve.reply["/api/sessions"] = `{"error":"serve: api: unknown project \"nope\""}`
	if got := r.run(t, `tools.spawn("t", {background: true, project: "nope"})`); got != `THREW workers: unknown project "nope"` {
		t.Fatalf("project = %s", got)
	}
	r.serve.codes["/api/sessions"] = 500
	r.serve.reply["/api/sessions"] = `{"error":"disk full"}`
	if got := r.run(t, `tools.spawn("t", {background: true})`); got != "THREW workers: background spawn: disk full" {
		t.Fatalf("other = %s", got)
	}
}

func TestAgentAndStopAgent(t *testing.T) {
	t.Parallel()
	r := bgMount(t, true, nil, nil)
	r.serve.reply["/api/sessions/c1/agent"] = `{"status":"done","title":"t","reply":"full reply","project":"demo","spawnedBy":"sess-parent"}`
	var st map[string]any
	if got := r.run(t, `tools.agent("c1")`); json.Unmarshal([]byte(got), &st) != nil || len(st) != 4 ||
		st["status"] != "done" || st["title"] != "t" || st["reply"] != "full reply" || st["project"] != "demo" {
		t.Fatalf("agent = %s", got)
	}
	if b := r.serve.body("/api/sessions/c1/agent"); b != "parent=sess-parent" {
		t.Fatalf("agent query = %q", b)
	}
	r.serve.codes["/api/sessions/c2/agent"] = 403
	r.serve.reply["/api/sessions/c2/agent"] = `{"error":"no"}`
	if got := r.run(t, `tools.agent("c2")`); got != `THREW workers: agent "c2" was not started by this session` {
		t.Fatalf("foreign = %s", got)
	}
	r.serve.codes["/api/sessions/c3/agent"] = 404
	r.serve.reply["/api/sessions/c3/agent"] = `{"error":"no"}`
	if got := r.run(t, `tools.agent("c3")`); got != `THREW workers: no agent "c3"` {
		t.Fatalf("unknown = %s", got)
	}

	r.serve.reply["/api/sessions/c1/stop"] = `{"ok":true,"was":"queued"}`
	if got := r.run(t, `tools.stopAgent("c1")`); got != `"stopped"` {
		t.Fatalf("stop = %s", got)
	}
	if b := r.serve.body("/api/sessions/c1/stop"); !strings.Contains(b, `"parent":"sess-parent"`) {
		t.Fatalf("stop body = %q", b)
	}
	r.serve.reply["/api/sessions/c1/stop"] = `{"ok":true,"was":"idle"}`
	if got := r.run(t, `tools.stopAgent("c1")`); got != `"not running"` {
		t.Fatalf("idle stop = %s", got)
	}
}

// A background agent runs on the parent's current model, not the
// config default: a child on a provider without a key failed at once.
func TestBackgroundSpawnInheritsModel(t *testing.T) {
	t.Parallel()
	rows := []kernel.Row{{ID: "llm", Plugin: "llm-openrouter", Config: map[string]any{"model": "z-ai/glm-5"}, Disabled: true}}
	r := bgMount(t, true, func(k *kernel.Context) {
		if err := k.Mount(rows); err != nil {
			t.Fatal(err)
		}
	}, nil)
	r.serve.codes["/api/sessions"] = 201
	r.serve.reply["/api/sessions"] = `{"session":{"id":"c"},"queued":false}`
	var req map[string]any
	r.run(t, `tools.spawn("t", {background: true})`)
	json.Unmarshal([]byte(r.serve.body("/api/sessions")), &req)
	if req["model"] != "llm-openrouter/z-ai/glm-5" {
		t.Fatalf("model not inherited: %v", req)
	}
	r.run(t, `tools.spawn("t", {background: true, model: "llm-anthropic/claude-sonnet-5"})`)
	json.Unmarshal([]byte(r.serve.body("/api/sessions")), &req)
	if req["model"] != "llm-anthropic/claude-sonnet-5" {
		t.Fatalf("opts.model not honoured: %v", req)
	}
}
