//go:build !windows

// Package engine is the engine-unreal row (go/docs/unreal-engine.md
// §6.1): the loop row's plugin swapped for one unreal-agent coordinator
// per bough session. It is kernel glue only. The session itself —
// store, operation manager, Gate, actor, projection — is
// internal/unreal/session; this row gives it the kernel's services as
// funcs read at the moment they are needed and provides the keys the
// loop provides, with the loop's types, so ui, serve, the web app and
// commands run unchanged at that seam.
package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/schema"
	"github.com/andreylukin/bough/internal/unreal/hookbridge"
	"github.com/andreylukin/bough/internal/unreal/prompt"
	"github.com/andreylukin/bough/internal/unreal/session"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

const name = "engine-unreal"

func init() {
	kernel.Register(name, func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

func (plugin) Name() string { return name }

// Inject is only what Apply cannot do without. Every other service is
// read lazily, off the apply goroutine: a Get during Apply is a remount
// edge, and a remount for /model or a hooks reload would otherwise
// rebuild what should outlive it.
func (plugin) Inject() []string { return []string{"history", "agent-tools"} }

// closeWait bounds how long an unmount waits for a turn in flight to
// record its cancelled/done: main.go's SIGINT path allows 3s in all.
const closeWait = 3 * time.Second

// handoffs holds a disposed mount's session for the next mount of the
// row, keyed by the kernel context (one per process, one per test). A
// remount on the same history file takes it over; any other remount
// closes it (the loop's pattern, loop.go handoffs).
var handoffs sync.Map // *kernel.Context -> *sess

// sess is one Runtime plus what it reads through; it outlives mounts.
type sess struct {
	kctx  *kernel.Context
	rt    *session.Runtime
	path  string
	cfg   settings
	hist  *liveHistory
	tools *liveRegistry
	secs  atomic.Pointer[loop.Sections]
	muted atomic.Bool
	noted atomic.Bool // the keep_whole_results note was shown
}

func remounting(kctx *kernel.Context) bool {
	for _, rs := range kctx.Rows() {
		if rs.Plugin == name && rs.State == kernel.StateActive {
			return true
		}
	}
	return false
}

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	st, err := readSettings(cfg)
	if err != nil {
		return err
	}
	h, err := kernel.Get[loop.History](kctx, "history")
	if err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	reg, err := kernel.Get[agenttools.Registry](kctx, "agent-tools")
	if err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}

	var s *sess
	if v, ok := handoffs.LoadAndDelete(kctx); ok {
		old := v.(*sess)
		if old.path == h.Path() && same(old.cfg, st) {
			s = old
			s.hist.set(h)
			s.tools.set(reg)
		} else {
			old.retire(h)
		}
	}
	if s == nil {
		if s, err = openSess(kctx, h, reg, st); err != nil {
			return err
		}
	}

	secs := &loop.Sections{}
	s.secs.Store(secs)
	kctx.Provide("prompt-sections", secs)
	kctx.Provide("runner", &runner{s: s})
	if creg, err := kernel.Get[*commands.Registry](kctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "context", Summary: "show everything injected into the system prompt"}
		if err := creg.Register(info, func(string) (string, error) { return s.rt.Context(), nil }); err != nil {
			return fmt.Errorf("%s: %w", name, err)
		}
		kctx.Effect(func() { creg.Unregister("context") })
	}
	inputs := make(chan string, 8)
	kctx.Provide("inputs", inputs)
	kctx.Provide("cancel", s.rt.Cancel)
	kctx.Provide("steer", s.rt.Steer)
	kctx.Provide("engine", engineKey{s})
	kctx.Provide("drain", s.rt.Drain)

	// The mount's own goroutines: the inputs chan and the stored-notice
	// poll. Both end with the mount; the session does not.
	stop := make(chan struct{})
	go func() {
		for line := range inputs {
			if s.cfg.keepWhole && s.noted.CompareAndSwap(false, true) {
				s.emit("system", "engine-unreal: keep_whole_results is ignored; the engine's context is append-only and nothing is trimmed", nil)
			}
			s.rt.Submit(line)
		}
	}()
	go pollStoredNotices(s.hist, s.notifier, stop)

	kctx.Effect(func() {
		close(stop)
		close(inputs)
		if remounting(kctx) {
			handoffs.Store(kctx, s)
			return
		}
		s.close()
		s.stopJobs()
	})
	return nil
}

// same reports whether a remount keeps the row's config: a changed
// config is a new Runtime, because the session reads its config once.
func same(a, b settings) bool { return reflect.DeepEqual(a, b) }

func openSess(kctx *kernel.Context, h loop.History, reg agenttools.Registry, st settings) (*sess, error) {
	s := &sess{kctx: kctx, path: h.Path(), cfg: st, hist: &liveHistory{h: h}, tools: newLiveRegistry(reg)}
	cwd, _ := os.Getwd()
	id := strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	bridge := hookbridge.New(func() (hookbridge.Firer, bool) {
		f, err := kernel.Get[hookbridge.Firer](kctx, "hooks")
		return f, err == nil
	})
	bridge.Session = id
	bridge.Notify = func(kind, text string) { s.emit(kind, text, nil) }
	d := session.Deps{
		SessionID:   id,
		HistoryPath: h.Path(),
		Store:       st.Store,
		Scratch:     s.scratch,
		Cwd:         cwd,
		Config:      st.Config,
		LLM:         s.llm,
		Usage:       s.usage,
		Tools:       s.tools,
		Hooks:       func() agenttools.Hooks { return bridge },
		Lifecycle:   func() session.Lifecycle { return bridge },
		Prompt:      s.parts,
		Orb:         s.orb,
		Redact:      s.redact,
		Jobs:        s.jobs,
		Stats:       lazy[session.TurnStats](kctx, "turn-stats"),
		Checkpoints: lazy[session.Checkpointer](kctx, "checkpoints"),
		Skills:      lazy[session.Skills](kctx, "skills"),
		History:     s.hist,
		Emit:        s.emit,
		Schema:      lazy[schema.Schema](kctx, "stop-schema"),
	}
	rt, err := session.Open(context.Background(), d)
	if err != nil {
		s.tools.close()
		return nil, err
	}
	s.rt = rt
	return s, nil
}

// retire ends a session the row is no longer running: the history row
// moved to another file (/new, /sessions) or the row's config changed.
// A different file means its last events belong to a pane that is gone,
// so they are muted, and its old Store is already closed, so the
// cancelled/done of a turn in flight go through a fresh handle on its
// own file instead of being lost.
func (s *sess) retire(now loop.History) {
	if s.path != now.Path() {
		s.muted.Store(true)
		if h, err := history.OpenExisting(s.path); err == nil {
			s.hist.set(h)
			defer h.Close()
		}
	} else {
		s.hist.set(now)
	}
	s.close()
}

// close ends the session: the turn in flight is cancelled and
// recorded, and Run stops.
func (s *sess) close() {
	ctx, cancel := context.WithTimeout(context.Background(), closeWait)
	defer cancel()
	_ = s.rt.Close(ctx)
	s.tools.close()
}

// stopJobs is the process going away: background jobs die with it,
// said in history so a resumed model is not left waiting for their
// news. Not on retire — job-notices then already serves the next
// session.
func (s *sess) stopJobs() {
	if j, err := kernel.Get[any](s.kctx, "job-notices"); err == nil {
		if st, ok := j.(interface{ Stop() }); ok {
			st.Stop()
			if t, ok := j.(interface{ Take() []string }); ok {
				for _, text := range t.Take() {
					s.hist.Append("job", map[string]any{"text": text})
				}
			}
		}
	}
}

// emit is every live event the session sends, as the loop.Event the
// title, activity, workers and cmux rows type-assert.
func (s *sess) emit(kind, text string, data map[string]any) {
	if s.muted.Load() {
		return
	}
	s.kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text, Data: data})
}

// lazy is an optional service read at use, nil when absent.
func lazy[T any](kctx *kernel.Context, key string) func() T {
	return func() T {
		v, _ := kernel.Get[T](kctx, key)
		return v
	}
}

// llm is the llm row as it is now: /model and /think swap or change it
// without restarting anything, so it is read per model request. The
// provenance name is the row's plugin, as the loop records it.
func (s *sess) llm() (agentllm.Source, string, error) {
	var plugin string
	for _, row := range s.kctx.Desired() {
		if row.ID == "llm" {
			plugin = row.Plugin
		}
	}
	v, err := kernel.Get[any](s.kctx, "llm")
	if err != nil {
		return nil, "", fmt.Errorf("%s: no llm row is mounted", name)
	}
	src, ok := v.(agentllm.Source)
	if !ok {
		return nil, "", fmt.Errorf("%s: the llm row (%s) cannot drive the engine: it has no agent adapter", name, orUnknown(plugin))
	}
	return src, strings.TrimPrefix(plugin, "llm-"), nil
}

func orUnknown(s string) string {
	if s == "" {
		return "unknown plugin"
	}
	return s
}

// usage is the cost row's tally, else the llm row's own; zero without
// either, which leaves done.usage out as the loop does.
func (s *sess) usage() llm.Usage {
	if u, err := kernel.Get[llm.UsageReporter](s.kctx, "usage"); err == nil {
		return u.Usage()
	}
	if u, err := kernel.Get[llm.UsageReporter](s.kctx, "llm"); err == nil {
		return u.Usage()
	}
	return llm.Usage{}
}

// jobs is tools-basic's job-notices. A tools row without Adopt still
// wakes and notifies; the session then numbers adopted calls itself.
func (s *sess) jobs() session.Jobs {
	v, err := kernel.Get[any](s.kctx, "job-notices")
	if err != nil {
		return nil
	}
	switch j := v.(type) {
	case session.Jobs:
		return j
	case loop.Notices:
		return noAdopt{j}
	}
	return nil
}

type noAdopt struct{ loop.Notices }

func (noAdopt) Adopt(string, string, func()) (int, func(*int, bool)) { return 0, nil }

// notifier is job-notices' Notify, for stored notices; nil without it.
func (s *sess) notifier() func(string) {
	v, err := kernel.Get[any](s.kctx, "job-notices")
	if err != nil {
		return nil
	}
	if n, ok := v.(interface{ Notify(string) }); ok {
		return n.Notify
	}
	return nil
}

func (s *sess) scratch() string {
	if p, err := kernel.Get[interface{ Dir() string }](s.kctx, "scratch"); err == nil {
		return p.Dir()
	}
	return os.Getenv("BOUGH_SCRATCH")
}

func (s *sess) orb() (string, bool) {
	o, err := kernel.Get[interface{ Root() string }](s.kctx, "orb")
	if err != nil {
		return "", false
	}
	return o.Root(), true
}

// redact is the orb's secret redactor, identity on the host.
func (s *sess) redact(text string) string {
	o, err := kernel.Get[interface{ Redactor() *iorb.Redactor }](s.kctx, "orb")
	if err != nil {
		return text
	}
	if r := o.Redactor(); r != nil {
		return r.String(text)
	}
	return text
}

// schemaSection is the engine's form of loop.SchemaSection: there is
// no stop block here, the final reply itself is the answer.
const schemaSection = `This turn's answer is STRUCTURED. End the turn with a final reply that is JSON matching this schema and nothing else — no prose around it, no code fence:

%s

Everything you want to say goes in the JSON's own fields. An answer that does not match is handed back to you with the mismatches.`

// parts is everything the prompt is composed from except the preamble
// and guidance (row config the session adds): read at every build and
// every Submit, so a drift becomes a <context-update>.
func (s *sess) parts() prompt.Parts {
	var p prompt.Parts
	if _, err := kernel.Get[any](s.kctx, "ask-answers"); err == nil {
		p.Ask = prompt.AskSection
	}
	if v, err := kernel.Get[any](s.kctx, "context-md"); err == nil {
		var files []struct{ Path, Text string }
		if call(v, "Parts", &files) {
			for _, f := range files {
				p.Context = append(p.Context, prompt.Part{Name: f.Path, Text: f.Text})
			}
		} else if pre, ok := v.(interface{ Preamble() string }); ok && pre.Preamble() != "" {
			p.Context = []prompt.Part{{Name: "context files", Text: pre.Preamble()}}
		}
	}
	if secs := s.secs.Load(); secs != nil {
		for _, n := range secs.Names() {
			if t := secs.Get(n); strings.TrimSpace(t) != "" {
				p.Sections = append(p.Sections, prompt.Part{Name: n, Text: t})
			}
		}
	}
	if v, err := kernel.Get[any](s.kctx, "skills"); err == nil {
		call(v, "Listing", &p.Skills)
	}
	if sc, err := kernel.Get[schema.Schema](s.kctx, "stop-schema"); err == nil && len(sc) > 0 {
		p.Schema = fmt.Sprintf(schemaSection, sc.Describe())
	}
	return p
}

// call runs v's no-argument method and decodes its single result into
// out through JSON. context-md's Parts and skills' Listing return types
// of their own packages, and a plugin never imports another plugin to
// name them; the field names are the contract.
func call(v any, method string, out any) bool {
	m := reflect.ValueOf(v).MethodByName(method)
	if !m.IsValid() || m.Type().NumIn() != 0 || m.Type().NumOut() != 1 {
		return false
	}
	b, err := json.Marshal(m.Call(nil)[0].Interface())
	if err != nil {
		return false
	}
	return json.Unmarshal(b, out) == nil
}

// engineKey is the "engine" service (§5.8), with plain types so a
// reader declares it structurally and never imports internal/unreal.
type engineKey struct{ s *sess }

func (e engineKey) Session() string   { return e.s.rt.SessionID() }
func (e engineKey) StorePath() string { return e.s.rt.StorePath() }

func (e engineKey) Spawn(ctx context.Context, task, worker, system string, maxSteps int) (string, string, int, error) {
	res, err := e.s.rt.Children().Run(ctx, session.ChildRequest{Task: task, Worker: worker, System: system, MaxSteps: maxSteps})
	return res.Reply, res.Status, res.Steps, err
}

func (e engineKey) CancelCall(callID string) error { return e.s.rt.CancelCall(callID) }

// runner is the "runner" key: /context, and the loop's Run seam — one
// input, its events, back when its turn's done lands.
type runner struct{ s *sess }

func (r *runner) Context() string { return r.s.rt.Context() }

func (r *runner) Run(ctx context.Context, input string, emit func(kind, text string)) error {
	done := make(chan struct{})
	var once sync.Once
	off := r.s.kctx.On("loop/event", func(p any) {
		ev, ok := p.(loop.Event)
		if !ok {
			return
		}
		emit(ev.Kind, ev.Text)
		if wake, _ := ev.Data["wake"].(bool); ev.Kind == "done" && !wake {
			once.Do(func() { close(done) })
		}
	})
	defer off()
	r.s.rt.Submit(input)
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		r.s.rt.Cancel()
		return ctx.Err()
	}
}
