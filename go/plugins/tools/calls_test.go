package tools

import (
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/loop"
)

// callsRunner mounts tools over a codemode with a history-record sink
// and a loop/event listener, returning both captures.
func callsRunner(t *testing.T) (cm *codemode.CodeMode, events func() []loop.Event, recorded func() []map[string]any, setSub func(bool)) {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	provideHostProject(ctx)
	var mu sync.Mutex
	var evs []loop.Event
	var recs []map[string]any
	sub := false
	ctx.Provide("history-record", func(kind string, data map[string]any) {
		mu.Lock()
		defer mu.Unlock()
		d := map[string]any{"kind": kind}
		for k, v := range data {
			d[k] = v
		}
		recs = append(recs, d)
	})
	ctx.Provide("subagent-worker", func() int {
		mu.Lock()
		defer mu.Unlock()
		if sub {
			return 3
		}
		return 0
	})
	ctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok {
			mu.Lock()
			evs = append(evs, ev)
			mu.Unlock()
		}
	})
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, err := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}
	return cm,
		func() []loop.Event { mu.Lock(); defer mu.Unlock(); return append([]loop.Event(nil), evs...) },
		func() []map[string]any { mu.Lock(); defer mu.Unlock(); return append([]map[string]any(nil), recs...) },
		func(v bool) { mu.Lock(); sub = v; mu.Unlock() }
}

// Every foreground call announces itself live (phase start) and lands
// once in the history with its evidence: the tool, an id, how long it
// took, a command's exit, an edit's added and removed lines.
func TestCallEventsPerTool(t *testing.T) {
	cm, events, recorded, _ := callsRunner(t)
	path := filepath.Join(t.TempDir(), "f.txt")
	code := `tools.bash("echo hi"); tools.write(` + jsStr(path) + `, "a\nb\n"); tools.patch(` + jsStr(path) + `, "b\n", "c\nd\n"); tools.view(` + jsStr(path) + `, 1, 2)`
	if _, err := cm.Run(code); err != nil {
		t.Fatalf("run: %v", err)
	}
	recs := recorded()
	if len(recs) != 4 {
		t.Fatalf("want 4 recorded calls, got %d: %v", len(recs), recs)
	}
	wantTool := []string{"bash", "write", "patch", "view"}
	wantText := []string{"echo hi", path, path, path + ":1-2"}
	for i, r := range recs {
		if r["kind"] != "call" || r["tool"] != wantTool[i] || r["text"] != wantText[i] {
			t.Errorf("call %d: %v", i, r)
		}
		if _, ok := r["ms"].(int64); !ok {
			t.Errorf("call %d: no ms: %v", i, r)
		}
		if r["id"] != i+1 {
			t.Errorf("call %d: id %v", i, r["id"])
		}
		if _, failed := r["error"]; failed {
			t.Errorf("call %d recorded an error: %v", i, r)
		}
	}
	if recs[0]["exit"] != 0 {
		t.Errorf("bash exit: %v", recs[0])
	}
	if recs[1]["add"] != 2 || recs[1]["del"] != 0 {
		t.Errorf("write counts: %v", recs[1])
	}
	if recs[2]["add"] != 2 || recs[2]["del"] != 1 {
		t.Errorf("patch counts: %v", recs[2])
	}
	// Live: a start before each end, in order, and the start is not recorded.
	var seen []string
	for _, ev := range events() {
		if ev.Kind != "call" {
			continue
		}
		seen = append(seen, ev.Data["tool"].(string)+":"+phase(ev))
	}
	want := "bash:start bash:end write:start write:end patch:start patch:end view:start view:end"
	if got := strings.Join(seen, " "); got != want {
		t.Errorf("live events\n got %s\nwant %s", got, want)
	}
}

func phase(ev loop.Event) string {
	if ev.Data["phase"] == "start" {
		return "start"
	}
	return "end"
}

// A failing call records what it said, without the tool's own prefix,
// so a row can lead with the reason.
func TestCallEventFailure(t *testing.T) {
	cm, _, recorded, _ := callsRunner(t)
	_, err := cm.Run(`tools.bash("exit 3")`)
	if err == nil {
		t.Fatal("want the bash failure")
	}
	recs := recorded()
	if len(recs) != 1 || recs[0]["exit"] != 3 {
		t.Fatalf("want one failed call with exit 3: %v", recs)
	}
	msg, _ := recs[0]["error"].(string)
	if !strings.HasPrefix(msg, "exit 3: exit status 3") {
		t.Errorf("error: %q", msg)
	}
}

// Inside a subagent the same calls are the child's: "sub:call", so a
// parent's transcript keeps them under the subagent card.
func TestCallEventsInSubagent(t *testing.T) {
	cm, events, recorded, setSub := callsRunner(t)
	setSub(true)
	if _, err := cm.Run(`tools.bash("true")`); err != nil {
		t.Fatal(err)
	}
	if recs := recorded(); len(recs) != 1 || recs[0]["kind"] != "sub:call" || recs[0]["worker"] != 3 {
		t.Errorf("recorded: %v", recs)
	}
	for _, ev := range events() {
		if ev.Kind == "call" {
			t.Errorf("a child's call reached the parent as %q", ev.Kind)
		}
	}
}

// A background job is not a call: it has job rows of its own.
func TestCallEventsSkipBackgroundJobs(t *testing.T) {
	cm, _, recorded, _ := callsRunner(t)
	if _, err := cm.Run(`tools.bash("true", 5)`); err != nil {
		t.Fatal(err)
	}
	for _, r := range recorded() {
		if r["kind"] == "call" {
			t.Errorf("a background job was recorded as a call: %v", r)
		}
	}
}

// The tools row's job_grace / job_settle set the foreground wait and
// the status settle; a bad value fails the row with its name.
func TestJobGraceConfig(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	provideHostProject(ctx)
	if err := (plugin{}).Apply(ctx, map[string]any{"job_grace": "0s", "job_settle": "1500ms"}); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	if st.jobs.grace != 0 || st.jobs.settleFor != 1500*time.Millisecond {
		t.Fatalf("grace=%s settle=%s", st.jobs.grace, st.jobs.settleFor)
	}
	bad := kernel.NewContext()
	bad.Provide("codemode", codemode.New(5*time.Second))
	provideHostProject(bad)
	if err := (plugin{}).Apply(bad, map[string]any{"job_grace": "soon"}); err == nil || !strings.Contains(err.Error(), "tools-basic: job_grace") {
		t.Fatalf("bad duration accepted: %v", err)
	}
}
