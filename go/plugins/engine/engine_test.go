package engine

import (
	"context"
	"encoding/json"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"

	_ "github.com/andreylukin/bough/plugins/agenttools"
	_ "github.com/andreylukin/bough/plugins/commands"
	_ "github.com/andreylukin/bough/plugins/llm"
)

// rig is the row mounted the way bough.yml mounts it under
// `--set loop.plugin=engine-unreal`: history, agent-tools, commands,
// the echo llm and the loop row on this plugin. Everything lives in
// the test's temp dirs; nothing reads $HOME.
type rig struct {
	t    *testing.T
	ctx  *kernel.Context
	path string

	mu  sync.Mutex
	evs []loop.Event
}

func newRig(t *testing.T, before func(*kernel.Context)) *rig {
	t.Helper()
	dir := t.TempDir()
	r := &rig{t: t, ctx: kernel.NewContext(), path: filepath.Join(dir, "01TEST.jsonl")}
	r.ctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok {
			r.mu.Lock()
			r.evs = append(r.evs, ev)
			r.mu.Unlock()
		}
	})
	if before != nil {
		before(r.ctx)
	}
	err := r.ctx.Mount([]kernel.Row{
		{ID: "history", Plugin: "history", Config: map[string]any{"file": r.path}},
		{ID: "commands", Plugin: "commands"},
		{ID: "agent-tools", Plugin: "agent-tools"},
		{ID: "llm", Plugin: "llm-echo"},
		{ID: "loop", Plugin: name, Config: map[string]any{"store": filepath.Join(dir, "engine")}},
	})
	if err != nil {
		t.Fatalf("mount: %v", err)
	}
	t.Cleanup(r.ctx.Unmount)
	for _, rs := range r.ctx.Rows() {
		if rs.State != kernel.StateActive {
			t.Fatalf("row %s is %v: %v %v", rs.ID, rs.State, rs.Err, rs.Missing)
		}
	}
	return r
}

func (r *rig) send(line string) {
	r.t.Helper()
	in, err := kernel.Get[chan string](r.ctx, "inputs")
	if err != nil {
		r.t.Fatal(err)
	}
	in <- line
}

func (r *rig) events(kind string) []loop.Event {
	r.mu.Lock()
	defer r.mu.Unlock()
	var out []loop.Event
	for _, e := range r.evs {
		if e.Kind == kind {
			out = append(out, e)
		}
	}
	return out
}

func (r *rig) entries() []history.Entry {
	h, err := kernel.Get[loop.History](r.ctx, "history")
	if err != nil {
		r.t.Fatal(err)
	}
	return h.Entries()
}

func (r *rig) count(kind string) int {
	n := 0
	for _, e := range r.entries() {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

func (r *rig) waitDone(n int) {
	r.t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for len(r.events("done")) < n {
		if time.Now().After(deadline) {
			r.t.Fatalf("waited for %d dones, have %d; history: %v", n, len(r.events("done")), kinds(r.entries()))
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func kinds(es []history.Entry) []string {
	var out []string
	for _, e := range es {
		out = append(out, e.Kind)
	}
	return out
}

func (r *rig) lastAssistant() string {
	evs := r.events("assistant")
	if len(evs) == 0 {
		return ""
	}
	return evs[len(evs)-1].Text
}

// The keys are what ui, main.go, workers and headless read, with the
// exact types they read them as: a look-alike type is a silent miss.
func TestProvidesTheLoopKeys(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	check := func(key string, get func() error) {
		t.Helper()
		if err := get(); err != nil {
			t.Errorf("%s: %v", key, err)
		}
	}
	check("inputs", func() error { _, err := kernel.Get[chan string](r.ctx, "inputs"); return err })
	check("cancel", func() error { _, err := kernel.Get[func()](r.ctx, "cancel"); return err })
	check("steer", func() error { _, err := kernel.Get[func(string) bool](r.ctx, "steer"); return err })
	check("drain", func() error { _, err := kernel.Get[func(context.Context) error](r.ctx, "drain"); return err })
	check("prompt-sections", func() error { _, err := kernel.Get[*loop.Sections](r.ctx, "prompt-sections"); return err })
	check("runner", func() error {
		_, err := kernel.Get[interface {
			Run(ctx context.Context, input string, emit func(kind, text string)) error
			Context() string
		}](r.ctx, "runner")
		return err
	})
	check("engine", func() error {
		_, err := kernel.Get[interface {
			Session() string
			StorePath() string
			Spawn(ctx context.Context, task, worker, system string, maxSteps int) (reply, status string, steps int, err error)
			CancelCall(callID string) error
		}](r.ctx, "engine")
		return err
	})
	// No turn open: a steer is refused, so ui sends the line as input.
	steer, _ := kernel.Get[func(string) bool](r.ctx, "steer")
	if steer("now") {
		t.Error("steer with no turn open returned true")
	}
}

// A line on the inputs chan is one bough turn end to end on the echo
// adapter: input, the quiet engine entry, the reply, one done.
func TestTextTurnThroughInputs(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	r.send("hello there")
	r.waitDone(1)
	if got := r.lastAssistant(); got != "echo: hello there" {
		t.Fatalf("assistant = %q", got)
	}
	want := []string{"input", "engine", "assistant", "done"}
	var got []string
	for _, k := range kinds(r.entries()) {
		if slices.Contains(want, k) {
			got = append(got, k)
		}
	}
	if !slices.Equal(got, want) {
		t.Fatalf("history kinds = %v, want %v", got, want)
	}
	if len(r.events("assistant-delta")) == 0 {
		t.Error("no assistant-delta events: the Gate's sink did not reach the ui")
	}
}

// The echo smoke from the design (§18.2 W2 "done when"): CODE! makes
// the echo model call bash natively, and its result comes back as the
// reply. The bash here is a test tool in the agent-tools registry,
// which is where tools-basic registers the real one.
func TestEchoSmokeRunsANativeCall(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	reg, err := kernel.Get[agenttools.Registry](r.ctx, "agent-tools")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := reg.Register(agenttools.Tool{
		Name: "bash", Description: "run a command",
		Schema: agenttools.Object([]string{"command"}, map[string]any{"command": agenttools.Prop("string", "the command")}),
		Detail: func(args json.RawMessage) string {
			var a struct{ Command string }
			_ = json.Unmarshal(args, &a)
			return a.Command
		},
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			return agenttools.Result{Text: "hi from codemode\n", Data: map[string]any{"exit": 0}}, nil
		},
	}); err != nil {
		t.Fatal(err)
	}
	r.send("CODE! please")
	r.waitDone(1)
	if got := r.lastAssistant(); got != "ran: hi from codemode" {
		t.Fatalf("assistant = %q; history: %v", got, kinds(r.entries()))
	}
	if r.count("call") != 1 {
		t.Fatalf("want one recorded call row, history: %v", kinds(r.entries()))
	}
}

// A remount of the history row (same file, new Store) remounts this
// row; the session and its coordinator carry over, so the second turn
// builds nothing and writes to the new Store.
func TestRemountOnTheSameFileKeepsTheSession(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	r.send("one")
	r.waitDone(1)
	eng, _ := kernel.Get[interface{ Session() string }](r.ctx, "engine")
	before := eng.Session()
	if err := r.ctx.Remount("history"); err != nil {
		t.Fatal(err)
	}
	eng2, err := kernel.Get[interface{ Session() string }](r.ctx, "engine")
	if err != nil {
		t.Fatal(err)
	}
	if eng2.Session() != before {
		t.Fatalf("session after remount = %q, want %q", eng2.Session(), before)
	}
	r.send("two")
	r.waitDone(2)
	if got := r.lastAssistant(); got != "echo: two" {
		t.Fatalf("assistant = %q", got)
	}
	es, err := history.ReadFile(r.path)
	if err != nil {
		t.Fatal(err)
	}
	var engines, dones int
	for _, e := range es {
		switch e.Kind {
		case "engine":
			engines++
		case "done":
			dones++
		}
	}
	if engines != 1 || dones != 2 {
		t.Fatalf("file has %d engine and %d done entries, want 1 and 2: %v", engines, dones, kinds(es))
	}
}

// /context works before the first input and reads prompt-sections
// through this mount's Sections.
func TestContextCommand(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	secs, err := kernel.Get[*loop.Sections](r.ctx, "prompt-sections")
	if err != nil {
		t.Fatal(err)
	}
	secs.Set("zz-test", "SECTION-MARKER")
	reg, err := kernel.Get[*commands.Registry](r.ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	out, err := reg.Run("context", "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "SECTION-MARKER") || !strings.Contains(out, "not frozen yet") {
		t.Fatalf("/context before any input:\n%s", out)
	}
}

// fakeNotices is tools-basic's job-notices without Adopt.
type fakeNotices struct {
	mu   sync.Mutex
	got  []string
	wake chan struct{}
}

func (f *fakeNotices) Notify(text string) {
	f.mu.Lock()
	f.got = append(f.got, text)
	f.mu.Unlock()
}
func (f *fakeNotices) Take() []string        { return nil }
func (f *fakeNotices) Wake() <-chan struct{} { return f.wake }

func (f *fakeNotices) texts() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.got)
}

// A notice serve appended to this file while the session ran is
// delivered once and marked delivered first.
func TestStoredNoticeDeliveredOnce(t *testing.T) {
	t.Parallel()
	no := &fakeNotices{wake: make(chan struct{})}
	r := newRig(t, func(c *kernel.Context) { c.Provide("job-notices", no) })
	h, err := kernel.Get[loop.History](r.ctx, "history")
	if err != nil {
		t.Fatal(err)
	}
	h.Append("notice", map[string]any{"id": "n1", "to": "01TEST", "text": "child finished"})
	h.Append("notice", map[string]any{"id": "n2", "to": "someone-else", "text": "not ours"})
	deadline := time.Now().Add(10 * time.Second)
	for len(no.texts()) == 0 {
		if time.Now().After(deadline) {
			t.Fatal("stored notice never delivered")
		}
		time.Sleep(20 * time.Millisecond)
	}
	time.Sleep(1500 * time.Millisecond) // one more poll must not re-deliver
	if got := no.texts(); !slices.Equal(got, []string{"child finished"}) {
		t.Fatalf("delivered %v", got)
	}
	if n := r.count("notice-delivered"); n != 1 {
		t.Fatalf("%d notice-delivered entries, want 1", n)
	}
}

func TestConfigErrorsNameTheRow(t *testing.T) {
	t.Parallel()
	bad := []map[string]any{
		{"tools": "js"},
		{"turn_settle": "soon"},
		{"turn_settle": 0},
		{"max_output": 2000000},
		{"max_steps": -1},
		{"stop_retries": 1.5},
		{"max_cost_usd": "lots"},
		{"steer_interrupts": "maybe"},
		{"system_prompt": " "},
		{"task_guidance": 3},
		{"store": ""},
	}
	for _, cfg := range bad {
		_, err := readSettings(cfg)
		if err == nil || !strings.HasPrefix(err.Error(), "engine-unreal: ") {
			t.Errorf("readSettings(%v) = %v, want an error naming the row", cfg, err)
		}
	}
	// A loop overlay's keys mount: unknown ones are ignored, and
	// keep_whole_results is accepted and noted.
	s, err := readSettings(map[string]any{"max_steps": 30, "effort": "high", "keep_whole_results": 4, "turn_settle": "2s"})
	if err != nil {
		t.Fatal(err)
	}
	if s.MaxSteps != 30 || !s.keepWhole || s.TurnSettle != 2*time.Second {
		t.Fatalf("settings = %+v", s)
	}
}

// Switching the history row to another file (/new, /sessions) retires
// the old session and opens one on the new file.
func TestAnotherFileIsAnotherSession(t *testing.T) {
	t.Parallel()
	r := newRig(t, nil)
	r.send("one")
	r.waitDone(1)
	eng, _ := kernel.Get[interface{ Session() string }](r.ctx, "engine")
	before := eng.Session()
	rows := r.ctx.Desired()
	next := filepath.Join(filepath.Dir(r.path), "02NEXT.jsonl")
	for i := range rows {
		if rows[i].ID == "history" {
			rows[i].Config = map[string]any{"file": next}
		}
	}
	if err := r.ctx.Reconcile(rows); err != nil {
		t.Fatal(err)
	}
	eng2, err := kernel.Get[interface{ Session() string }](r.ctx, "engine")
	if err != nil {
		t.Fatal(err)
	}
	if eng2.Session() == before || eng2.Session() != "02NEXT" {
		t.Fatalf("session after the switch = %q (was %q)", eng2.Session(), before)
	}
	r.send("two")
	r.waitDone(2)
	es, err := history.ReadFile(next)
	if err != nil {
		t.Fatal(err)
	}
	if got := kinds(es); !slices.Contains(got, "engine") || !slices.Contains(got, "done") {
		t.Fatalf("new file: %v", got)
	}
}
