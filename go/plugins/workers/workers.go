// Package workers is the "workers" plugin: subagents. It registers
// tools.spawn(task) into codemode; the model delegates a task to a
// bounded child agent run and gets its final plain-text reply back as
// the tool's return value (the normal logged result path — no side
// channel).
//
// The child's step loop is a small local copy of the loop plugin's
// (llm -> extract js -> run -> feed back): the loop's runner is welded
// to its hook/skill/projection seams and its own history, so extracting
// a shared core wasn't cheap — accepted v0. The child keeps its context
// as a plain message list (exactly what loop.DefaultProject would
// derive from a fresh history seeded with the task), and its activity
// is mirrored into the session history as "sub:*" entries so subagent
// transcripts replay. DefaultProject ignores unknown kinds, so sub:*
// entries never reach the parent's model context.
//
// VM note: the child's code blocks run in the same goja VM as the
// parent's (codemode's mutex is re-entrant for the tool goroutine, so
// they interleave synchronously rather than deadlock; globals are
// shared between parent and child JS). Accepted v0.
package workers

import (
	"context"
	"fmt"
	"regexp"
	"strconv"
	"sync"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// SubSystemPrompt is the child agent's system prompt.
const SubSystemPrompt = "You are a bough subagent. Complete exactly this task, using js code blocks with the tools API when needed, and reply with your final answer as plain text."

// promptSection documents tools.spawn to the parent model (registered
// into the loop's "prompt-sections" service when present).
const promptSection = `Subagents: tools.spawn(task) -> string runs a bounded child agent (same tools, fresh context, no nested spawns) on the task string and returns its final plain-text reply. Use it to delegate self-contained work, never a shell command.`

// sections is the slice of the loop's "prompt-sections" service we need.
type sections interface {
	Set(name, text string)
}

const defaultMaxSpawns = 4
const defaultMaxSteps = 6
const maxResultBytes = 8 * 1024

// jsBlock is a local copy of the loop plugin's fence matcher (unexported
// there).
var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// Codemode is the slice of the "codemode" service workers needs.
type Codemode interface {
	RegisterTool(name string, fn any)
	Run(code string) (string, error)
}

// History is the optional "history" service seam: only Append is needed
// (sub:* entries mirror the child's activity for replay).
type History interface {
	Append(kind string, data map[string]any) history.Entry
}

// Workers holds the spawn tool's state: the services a child run needs,
// the per-turn spawn counter, and the depth guard.
type Workers struct {
	mu        sync.Mutex
	llm       llm.LLM
	code      Codemode
	hist      History // nil when no "history" service is mounted
	emit      func(kind, text string, worker int)
	ctx       context.Context
	maxSpawns int
	maxSteps  int
	spawns    int  // spawns this parent turn; reset on the loop's "done"
	inChild   bool // a child run is active: no nested spawns
	nextID    int  // worker numbering, monotonic per session
}

// spawn is tools.spawn(task) -> final reply. A returned error becomes a
// JS exception in the calling code block.
func (w *Workers) spawn(task string) (string, error) {
	w.mu.Lock()
	if w.inChild {
		w.mu.Unlock()
		return "", fmt.Errorf("workers: subagent depth 1 only")
	}
	if w.spawns >= w.maxSpawns {
		w.mu.Unlock()
		return "", fmt.Errorf("workers: spawn limit reached (%d per turn)", w.maxSpawns)
	}
	w.spawns++
	w.nextID++
	id := w.nextID
	w.inChild = true
	w.mu.Unlock()
	defer func() {
		w.mu.Lock()
		w.inChild = false
		w.mu.Unlock()
	}()
	return w.runChild(task, id)
}

// runChild drives one child agent run: fresh context seeded with the
// task, up to maxSteps llm steps, js blocks executed via codemode. The
// final plain-text reply (no js block) is the result.
func (w *Workers) runChild(task string, id int) (string, error) {
	// note mirrors child activity: a "sub:<kind>" session-history entry
	// (when history is mounted) and a "loop/event" with the same kind,
	// both carrying the worker number.
	note := func(kind, text string) {
		if w.hist != nil {
			w.hist.Append("sub:"+kind, map[string]any{"text": text, "worker": id})
		}
		w.emit("sub:"+kind, text, id)
	}

	msgs := []llm.Message{{Role: "user", Content: task}}
	for step := 0; step < w.maxSteps; step++ {
		reply, err := w.llm.Complete(w.ctx, SubSystemPrompt, msgs)
		if err != nil {
			note("error", err.Error())
			note("done", "")
			return "", fmt.Errorf("workers: subagent llm: %w", err)
		}
		note("assistant", reply)
		msgs = append(msgs, llm.Message{Role: "assistant", Content: reply})
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			note("done", "")
			return reply, nil
		}
		for _, m := range blocks {
			code := m[1]
			note("code", code)
			out, runErr := w.code.Run(code)
			if runErr != nil {
				out = "error: " + runErr.Error()
				note("error", out)
			} else {
				out = truncate(out, maxResultBytes)
				note("result", out)
			}
			msgs = append(msgs, llm.Message{Role: "user", Content: "[tool output]\n" + out})
		}
	}
	err := fmt.Errorf("workers: subagent gave up after %d steps", w.maxSteps)
	note("error", err.Error())
	note("done", "")
	return "", err
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "\n[truncated]"
}

// intOpt reads an optional integer config key (yaml int or --set string).
func intOpt(cfg map[string]any, key string, def int) (int, error) {
	v, ok := cfg[key]
	if !ok {
		return def, nil
	}
	switch n := v.(type) {
	case int:
		return n, nil
	case int64:
		return int(n), nil
	case float64:
		if n == float64(int(n)) {
			return int(n), nil
		}
	case string:
		if i, err := strconv.Atoi(n); err == nil {
			return i, nil
		}
	}
	return 0, fmt.Errorf("workers: %s must be an integer, got %v", key, v)
}

type plugin struct{}

func init() {
	kernel.Register("workers", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "workers" }
func (plugin) Inject() []string { return []string{"llm", "codemode"} }

// Apply validates config {max_spawns, max_steps} and registers
// tools.spawn. Like tools-basic, the registered tool is not withdrawn
// on unmount (codemode has no UnregisterTool); a codemode remount
// re-registers cleanly.
func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "max_spawns" && k != "max_steps" {
			return fmt.Errorf("workers: unknown config key %q", k)
		}
	}
	maxSpawns, err := intOpt(cfg, "max_spawns", defaultMaxSpawns)
	if err != nil {
		return err
	}
	maxSteps, err := intOpt(cfg, "max_steps", defaultMaxSteps)
	if err != nil {
		return err
	}
	if maxSpawns < 1 || maxSteps < 1 {
		return fmt.Errorf("workers: max_spawns and max_steps must be >= 1 (got %d, %d)", maxSpawns, maxSteps)
	}

	l, err := kernel.Get[llm.LLM](kctx, "llm")
	if err != nil {
		return err
	}
	cm, err := kernel.Get[Codemode](kctx, "codemode")
	if err != nil {
		return err
	}
	w := &Workers{llm: l, code: cm, maxSpawns: maxSpawns, maxSteps: maxSteps}
	// Optional seam: without history, sub:* entries are events only.
	if h, err := kernel.Get[History](kctx, "history"); err == nil {
		w.hist = h
	}

	ctx, cancel := context.WithCancel(context.Background())
	w.ctx = ctx
	kctx.Effect(cancel)

	w.emit = func(kind, text string, worker int) {
		kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text, Data: map[string]any{"worker": worker}})
	}
	// The loop emits "done" at the end of every turn (all end paths);
	// that is the per-wake reset for the spawn counter.
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "done" {
			w.mu.Lock()
			w.spawns = 0
			w.mu.Unlock()
		}
	})

	cm.RegisterTool("spawn", w.spawn)
	// Optional seam: the loop's prompt-sections registry, so the model
	// learns tools.spawn exists. Withdrawn on unmount.
	if s, err := kernel.Get[sections](kctx, "prompt-sections"); err == nil {
		s.Set("workers", promptSection)
		kctx.Effect(func() { s.Set("workers", "") })
	}
	return nil
}
