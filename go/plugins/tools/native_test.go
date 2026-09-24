package tools

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// mountNative mounts tools-basic beside an agent-tools registry; setup
// provides whatever the session needs first (mode, orb).
func mountNative(t *testing.T, setup func(*kernel.Context)) (*kernel.Context, agenttools.Registry, *Stats) {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	reg := agenttools.NewRegistry()
	ctx.Provide("agent-tools", reg)
	if setup != nil {
		setup(ctx)
	}
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	return ctx, reg, st
}

// progress collects a call's streamed output.
type progress struct {
	mu     sync.Mutex
	chunks []string
}

func (p *progress) add(s string) { p.mu.Lock(); p.chunks = append(p.chunks, s); p.mu.Unlock() }
func (p *progress) text() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return strings.Join(p.chunks, "")
}

func call(t *testing.T, ctx context.Context, reg agenttools.Registry, name, id, args string, p *progress) agenttools.Result {
	t.Helper()
	tl, ok := reg.Lookup(name)
	if !ok {
		t.Fatalf("no native %s", name)
	}
	c := agenttools.Call{ID: id, Args: json.RawMessage(args)}
	if p != nil {
		c.Progress = p.add
	}
	r, err := tl.Call(ctx, c)
	if err != nil {
		t.Fatalf("%s: %v", name, err)
	}
	return r
}

func names(reg agenttools.Registry) string {
	var n []string
	for _, tl := range reg.Tools() {
		n = append(n, tl.Name)
	}
	return strings.Join(n, ",")
}

// The write-roots rule holds natively too: a local session with no
// roots has no write or patch to call, a project session has both, and
// unmounting the row takes every tool back out.
func TestNativeToolSetFollowsWriteRoots(t *testing.T) {
	t.Parallel()
	_, reg, _ := mountNative(t, nil)
	if got := names(reg); got != "bash,job,job_kill,jobs,view" {
		t.Fatalf("local tools = %s", got)
	}
	ctx, preg, _ := mountNative(t, provideHostProject)
	if got := names(preg); got != "bash,job,job_kill,jobs,patch,view,write" {
		t.Fatalf("project tools = %s", got)
	}
	for _, tl := range preg.Tools() {
		if !agenttools.ValidName(tl.Name) || tl.Description == "" || tl.Schema["type"] != "object" {
			t.Errorf("tool %s: name, description or schema missing", tl.Name)
		}
	}
	ctx.Unmount()
	if got := names(preg); got != "" {
		t.Fatalf("after unmount: %s", got)
	}
}

// Foreground bash streams its output to the call row while it runs, and
// a non-zero exit fails the call with the output still readable.
func TestNativeBashStreamsAndFails(t *testing.T) {
	t.Parallel()
	_, reg, st := mountNative(t, nil)
	var p progress
	r := call(t, context.Background(), reg, "bash", "c1", `{"command":"printf 'a\\n'; sleep 0.3; printf 'b\\n'; exit 3"}`, &p)
	if r.Error != "exit status 3" || r.Text != "a\nb\n" {
		t.Fatalf("result = %+v", r)
	}
	if r.Data["exit"] != 3 || r.Data["cmd"] == nil {
		t.Fatalf("data = %v", r.Data)
	}
	p.mu.Lock()
	n := len(p.chunks)
	p.mu.Unlock()
	if p.text() != "a\nb\n" || n < 2 {
		t.Fatalf("progress = %q in %d chunks, want a then b as they were printed", p.text(), n)
	}
	if _, exit, ran := st.Take(); !ran || exit != 3 {
		t.Fatalf("turn-stats exit = %d ran = %v", exit, ran)
	}
	ok := call(t, context.Background(), reg, "bash", "c2", `{"command":"echo fine"}`, nil)
	if ok.Error != "" || ok.Text != "fine\n" || ok.Data["exit"] != 0 {
		t.Fatalf("ok result = %+v", ok)
	}
}

// bash's own timeout kills the command and says so; a cancelled call
// (Esc) kills it too.
func TestNativeBashTimeoutAndCancel(t *testing.T) {
	t.Parallel()
	_, reg, _ := mountNative(t, nil)
	start := time.Now()
	r := call(t, context.Background(), reg, "bash", "t", `{"command":"echo started; sleep 30","timeout":0.3}`, nil)
	if r.Error != "bash: killed after 300ms" || !strings.Contains(r.Text, "started") || time.Since(start) > 10*time.Second {
		t.Fatalf("timeout result = %+v after %s", r, time.Since(start))
	}
	// Past the engine's call_timeout the error is the context's; boughcall
	// renames it after the timeout it applied.
	dctx, dcancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer dcancel()
	if r := call(t, dctx, reg, "bash", "d", `{"command":"sleep 30"}`, nil); r.Error != "bash: context deadline exceeded" {
		t.Fatalf("call-timeout result = %+v", r)
	}
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(200*time.Millisecond, cancel)
	if r := call(t, ctx, reg, "bash", "x", `{"command":"sleep 30"}`, nil); r.Error != "bash: cancelled" {
		t.Fatalf("cancel result = %+v", r)
	}
	for _, bad := range []string{`{"command":""}`, `{"command":"x","timeout":"soon"}`, `{"command":"x","until":"y"}`} {
		if r := call(t, context.Background(), reg, "bash", "b", bad, nil); r.Error == "" {
			t.Errorf("%s: no error", bad)
		}
	}
}

// background:true is the jobs.go path: a job id at once, no foreground
// grace, and the finish arrives as a notice.
func TestNativeBashBackground(t *testing.T) {
	t.Parallel()
	_, reg, st := mountNative(t, nil)
	start := time.Now()
	r := call(t, context.Background(), reg, "bash", "bg", `{"command":"echo quick","background":true}`, nil)
	if r.Error != "" || !strings.Contains(r.Text, "job 1 started in the background (limit 30m0s)") || r.Data["job"] != 1 {
		t.Fatalf("background result = %+v", r)
	}
	if d := time.Since(start); d > 5*time.Second {
		t.Fatalf("background call waited %s", d)
	}
	select {
	case <-st.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("finished job queued no notice")
	}
	if n := st.jobs.Take(); len(n) != 1 || !strings.Contains(n[0], "quick") {
		t.Fatalf("notices = %q", n)
	}
	out := call(t, context.Background(), reg, "job", "j", `{"id":1}`, nil)
	if !strings.Contains(out.Text, "exited 0") || !strings.Contains(out.Text, "quick") {
		t.Fatalf("job = %+v", out)
	}
	if r := call(t, context.Background(), reg, "job", "j", `{"id":9}`, nil); !strings.Contains(r.Error, "no job 9") {
		t.Fatalf("unknown job = %+v", r)
	}
}

// A project session never falls back to the host: with no orb, native
// bash and write refuse and nothing runs.
func TestNativeProjectWithoutOrbRefuses(t *testing.T) {
	t.Parallel()
	_, reg, _ := mountNative(t, func(ctx *kernel.Context) { ctx.Provide("session-mode", "project") })
	marker := filepath.Join(t.TempDir(), "ran")
	if r := call(t, context.Background(), reg, "bash", "c", `{"command":"touch `+marker+`"}`, nil); !strings.Contains(r.Error, "orb not ready") {
		t.Fatalf("bash = %+v", r)
	}
	if r := call(t, context.Background(), reg, "bash", "c", `{"command":"touch `+marker+`","background":true}`, nil); r.Error == "" {
		t.Fatalf("background bash = %+v", r)
	}
	if r := call(t, context.Background(), reg, "write", "w", `{"path":"`+marker+`","content":"x"}`, nil); !strings.Contains(r.Error, "orb not ready") {
		t.Fatalf("write = %+v", r)
	}
	if _, err := os.Stat(marker); err == nil {
		t.Fatal("a project session ran on the host with no orb")
	}
}

// In a project session bash runs through the orb, and the orb's secrets
// are redacted on the streamed row as well as in the result, even when
// the value arrives split across writes.
func TestNativeProjectBashRunsInOrbRedacted(t *testing.T) {
	t.Parallel()
	o := &redactOrb{fakeOrb{root: t.TempDir()}}
	_, reg, _ := mountNative(t, func(ctx *kernel.Context) {
		ctx.Provide("session-mode", "project")
		ctx.Provide("orb", orbExec(o))
	})
	var p progress
	r := call(t, context.Background(), reg, "bash", "c", `{"command":"echo in=$IN_ORB; for c in sk- live -012 3456 789; do printf %s $c; sleep 0.05; done; echo"}`, &p)
	if r.Error != "" || !strings.Contains(r.Text, "in=yes") {
		t.Fatalf("result = %+v", r)
	}
	for what, s := range map[string]string{"result": r.Text, "progress": p.text()} {
		if strings.Contains(s, "sk-live") || !strings.Contains(s, "[redacted:API_KEY]") {
			t.Errorf("%s = %q", what, s)
		}
	}
	if o.calls() != 1 {
		t.Fatalf("orb commands = %d", o.calls())
	}
}

// view, write and patch are the codemode functions: numbered lines,
// add/del counts on the row, and an image pointed at view_image.
func TestNativeViewWritePatch(t *testing.T) {
	t.Parallel()
	_, reg, st := mountNative(t, provideHostProject)
	dir := t.TempDir()
	f := filepath.Join(dir, "a.txt")
	w := call(t, context.Background(), reg, "write", "w", `{"path":"`+f+`","content":"one\ntwo\nthree\n"}`, nil)
	if w.Error != "" || w.Data["add"] != 3 || w.Data["path"] != f {
		t.Fatalf("write = %+v", w)
	}
	p := call(t, context.Background(), reg, "patch", "p", `{"path":"`+f+`","old":"two","new":"2\n2b"}`, nil)
	if p.Error != "" || p.Data["add"] != 2 || p.Data["del"] != 1 {
		t.Fatalf("patch = %+v", p)
	}
	v := call(t, context.Background(), reg, "view", "v", `{"path":"`+f+`","start":2,"end":3}`, nil)
	if v.Text != "2│2\n3│2b\n" {
		t.Fatalf("view = %q", v.Text)
	}
	tl, _ := reg.Lookup("view")
	if d := tl.Detail(json.RawMessage(`{"path":"` + f + `","start":2,"end":3}`)); d != f+":2-3" {
		t.Fatalf("view detail = %q", d)
	}
	if files, _, _ := st.Take(); len(files) != 1 || files[0] != f {
		t.Fatalf("turn-stats files = %v", files)
	}
	img := filepath.Join(dir, "shot.png")
	if err := os.WriteFile(img, []byte("\x89PNG"), 0o644); err != nil {
		t.Fatal(err)
	}
	if r := call(t, context.Background(), reg, "view", "i", `{"path":"`+img+`"}`, nil); r.Error != "" || !strings.Contains(r.Text, "call view_image") {
		t.Fatalf("image view = %+v", r)
	}
	if r := call(t, context.Background(), reg, "patch", "p", `{"path":"`+f+`","old":"nope","new":"x"}`, nil); !strings.Contains(r.Error, "old text not found") {
		t.Fatalf("bad patch = %+v", r)
	}
}

// The rules row's policy refuses a native command as it does a codemode one.
func TestNativeBashPolicy(t *testing.T) {
	t.Parallel()
	_, reg, st := mountNative(t, nil)
	st.SetPolicy(func(cmd string) error {
		if strings.Contains(cmd, "rm") {
			return errors.New("rules: rm is forbidden")
		}
		return nil
	})
	if r := call(t, context.Background(), reg, "bash", "c", `{"command":"rm -rf nothing"}`, nil); r.Error != "rules: rm is forbidden" {
		t.Fatalf("policy = %+v", r)
	}
}

// Adopt numbers an engine call as a job on the same counter background
// bash uses; the strip lists it, job reads its live output, job_kill
// reaches the engine's kill, and finish records the typed entry.
func TestAdoptSharesNumberingAndKills(t *testing.T) {
	t.Parallel()
	_, reg, st := mountNative(t, nil)
	// job and jobs settle on a running job for 10 s by default; here both
	// jobs are meant to be running (or just killed), so the settle only
	// padded the test by 20 s. It has its own test in jobs_test.go.
	st.jobs.settleFor = 0
	var mu sync.Mutex
	var recs []map[string]any
	st.jobs.record = func(kind string, data map[string]any) {
		mu.Lock()
		recs = append(recs, data)
		mu.Unlock()
	}
	if r := call(t, context.Background(), reg, "bash", "bg", `{"command":"sleep 30","background":true}`, nil); r.Data["job"] != 1 {
		t.Fatalf("background = %+v", r)
	}
	// A foreground call still running when the engine adopts it.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan agenttools.Result, 1)
	go func() {
		done <- call(t, ctx, reg, "bash", "call-7", `{"command":"echo building; sleep 30"}`, nil)
	}()
	waitFor(t, "the call's output", func() bool {
		st.jobs.mu.Lock()
		o := st.jobs.calls["call-7"]
		st.jobs.mu.Unlock()
		return o != nil && strings.Contains(o.text(), "building")
	})
	id, finish := st.jobs.Adopt("echo building; sleep 30", "call-7", cancel)
	if id != 2 {
		t.Fatalf("adopted id = %d, want 2", id)
	}
	if run := st.jobs.Running(); len(run) != 2 || run[1].ID != 2 {
		t.Fatalf("Running = %+v", run)
	}
	if out := call(t, context.Background(), reg, "job", "j", `{"id":2}`, nil); !strings.Contains(out.Text, "[running]") || !strings.Contains(out.Text, "building") {
		t.Fatalf("job 2 = %+v", out)
	}
	if out := call(t, context.Background(), reg, "job_kill", "k", `{"id":2}`, nil); out.Text != "job 2 killed" {
		t.Fatalf("job_kill = %+v", out)
	}
	if r := <-done; r.Error != "bash: cancelled" {
		t.Fatalf("killed call = %+v", r)
	}
	finish(nil, true)
	finish(nil, true) // only the first counts
	if out := call(t, context.Background(), reg, "jobs", "l", `{}`, nil); !strings.Contains(out.Text, "job 2 [killed]") {
		t.Fatalf("jobs = %q", out.Text)
	}
	if run := st.jobs.Running(); len(run) != 1 {
		t.Fatalf("Running after finish = %+v", run)
	}
	if n := st.jobs.Take(); len(n) != 0 {
		t.Fatalf("an adopted job queued a notice: %q", n)
	}
	mu.Lock()
	defer mu.Unlock()
	var adopted []map[string]any
	for _, r := range recs {
		if r["call"] == "call-7" {
			adopted = append(adopted, r)
		}
	}
	if len(adopted) != 2 || adopted[0]["event"] != "started" || adopted[1]["event"] != "finished" || adopted[1]["stopped"] != true || adopted[1]["id"] != 2 {
		t.Fatalf("adopted records = %v", adopted)
	}
	if _, has := adopted[1]["exit"]; has {
		t.Fatalf("a stopped call recorded an exit: %v", adopted[1])
	}
}

// /jobkill N, the person's way in, reaches an adopted call too.
func TestJobkillCommandStopsAdoptedCall(t *testing.T) {
	t.Parallel()
	st := newTestStats(t)
	killed := make(chan struct{})
	id, finish := st.jobs.Adopt("npm run dev", "c9", func() { close(killed) })
	if out, err := st.jobs.jobKill(id); err != nil || out != "job 1 killed" {
		t.Fatalf("jobKill = %q, %v", out, err)
	}
	select {
	case <-killed:
	case <-time.After(time.Second):
		t.Fatal("kill not called")
	}
	code := 0
	finish(&code, false)
	if out, _ := st.jobs.jobKill(id); out != "job 1 already finished" {
		t.Fatalf("second kill = %q", out)
	}
}
