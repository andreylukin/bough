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
	"maps"
	"os"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// SubSystemPrompt is the child agent's identity, appended to the same
// base prompt and live prompt sections the parent runs on: the child
// executes in the parent's codemode VM, so it must be told the same
// tools (bash, view, patch, mcp, ...) in the same words. Without this
// the child was told only that "the tools API" exists.
const SubSystemPrompt = `You are a bough subagent spawned for ONE task. Complete exactly that task with the tools above (you cannot spawn), then reply with a REPORT as plain text — no code block in the final reply. The report is all the parent agent sees, so make it self-contained and short (under 30 lines):

Status: ok | failed
Findings: what you established, as bullets (facts, numbers, paths)
Files: paths you changed, or "none"
Open: questions or blockers for the parent, or "none"`

// promptSection documents tools.spawn to the parent model (registered
// into the loop's "prompt-sections" service when present).
const promptSection = `Subagents:
- tools.spawn(task) -> string runs ONE bounded child agent (same tools, fresh context, no nested spawns) on the task string and returns its report.
- tools.spawnAll([task1, task2, …]) -> [report, …] runs several children AT ONCE and returns their reports in order. Prefer it whenever you have more than one independent task: the children wait on the model in parallel, so N tasks take about as long as the slowest one.
Both are synchronous — no await. Use them to delegate self-contained work, never a shell command. Limits: at most %d spawns per turn and %d steps per child, so give each child one well-scoped task and do small things yourself.`

// sections is the slice of the loop's "prompt-sections" service we need:
// Set to advertise tools.spawn to the parent, Text to hand the child the
// same tool documentation the parent has.
type sections interface {
	Set(name, text string)
	Text() string
	TextExcept(skip ...string) string
}

// systemFor builds the child's system prompt: the loop's base prompt,
// the live sections (minus this plugin's own spawn advert), then the
// subagent identity. Pure over its inputs.
func systemFor(base, sections string) string {
	s := base
	if sections != "" {
		s += "\n\n" + sections
	}
	s += "\n\n" + SubSystemPrompt
	if wd, err := os.Getwd(); err == nil {
		s += "\n\nYour working directory is " + wd + ". Every path in your task is relative to it unless it starts with /. If a path in the task does not exist, do not guess another one: run ls to find what is actually there, and say so in your report."
	}
	return s
}

const defaultMaxSpawns = 8
const defaultMaxSteps = 20
const maxResultBytes = 8 * 1024

// jsBlock is a local copy of the loop plugin's fence matcher (unexported
// there).
var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// Codemode is the slice of the "codemode" service workers needs.
type Codemode interface {
	RegisterTool(name string, fn any)
	Run(code string) (string, error)
}

// pauser is codemode's optional seam for a tool that blocks longer than
// the script timeout (tools.ask uses it too). Without it a child run
// ticks the PARENT's deadline and a slow child kills the block that
// spawned it — losing the child's finished work with it.
type pauser interface{ Pause() func() }

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
	hist      History                                      // nil when no "history" service is mounted
	secs      sections                                     // nil when the loop's prompt-sections are absent
	emit      func(kind, text string, data map[string]any) // data always carries "worker"
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
		return "", fmt.Errorf("workers: spawn limit reached (%d per turn) — do the remaining work yourself in this turn", w.maxSpawns)
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
	if p, ok := w.code.(pauser); ok {
		defer p.Pause()()
	}
	reply, err := w.runChild(task, id, w.code.Run, false)
	if err != nil {
		// A child the provider killed never got to work: refund its
		// slot, or a flaky minute burns the whole turn's budget.
		if strings.Contains(err.Error(), "subagent llm:") {
			w.mu.Lock()
			w.spawns--
			w.mu.Unlock()
		}
		return "", err
	}
	// Provenance: the parent (and the user reading its code output)
	// can tell delegated findings from the parent's own work.
	return fmt.Sprintf("[subagent %d · task: %s]\n%s", id, oneLine(task, 80), reply), nil
}

// oneLine is the first line of s, cut to n runes with an ellipsis.
func oneLine(s string, n int) string {
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i]
	}
	if r := []rune(s); len(r) > n {
		return string(r[:n]) + "…"
	}
	return s
}

// reportStatus reads the report's "Status:" line: "ok", "failed", or
// "" when the child did not follow the contract.
func reportStatus(report string) string {
	for ln := range strings.SplitSeq(report, "\n") {
		ln = strings.TrimSpace(strings.TrimLeft(ln, "*#- "))
		if v, ok := strings.CutPrefix(strings.ToLower(ln), "status:"); ok {
			v = strings.TrimSpace(strings.Trim(v, "*` "))
			switch {
			case strings.HasPrefix(v, "ok"):
				return "ok"
			case strings.HasPrefix(v, "fail"):
				return "failed"
			}
			return ""
		}
	}
	return ""
}

// codeReq is a child asking the parent's goroutine to run a block. The
// goja VM belongs to one goroutine, so concurrent children hand their
// code back to the owner and wait; their LLM calls, which is where the
// minutes go, overlap freely.
type codeReq struct {
	code string
	resp chan codeRes
}

type codeRes struct {
	out string
	err error
}

// spawnAll runs several children at once and returns their reports in
// the order the tasks were given. Each child's code executes on this
// goroutine (the VM owner); only the waiting overlaps.
func (w *Workers) spawnAll(tasks []string) ([]string, error) {
	if len(tasks) == 0 {
		return nil, fmt.Errorf("workers: spawnAll needs at least one task")
	}
	w.mu.Lock()
	if w.inChild {
		w.mu.Unlock()
		return nil, fmt.Errorf("workers: subagent depth 1 only")
	}
	if w.spawns+len(tasks) > w.maxSpawns {
		left := w.maxSpawns - w.spawns
		w.mu.Unlock()
		return nil, fmt.Errorf("workers: %d task(s) but only %d spawn(s) left this turn — send fewer, or do the rest yourself", len(tasks), left)
	}
	ids := make([]int, len(tasks))
	for i := range tasks {
		w.spawns++
		w.nextID++
		ids[i] = w.nextID
	}
	w.inChild = true
	w.mu.Unlock()
	defer func() {
		w.mu.Lock()
		w.inChild = false
		w.mu.Unlock()
	}()
	if p, ok := w.code.(pauser); ok {
		defer p.Pause()()
	}

	// Announce every child before any of them starts, so the cards read
	// 1..N down the transcript instead of in scheduling order.
	for i, task := range tasks {
		data := map[string]any{"text": task, "worker": ids[i]}
		if w.hist != nil {
			w.hist.Append("sub:start", data)
		}
		w.emit("sub:start", task, map[string]any{"worker": ids[i]})
	}

	reqCh := make(chan codeReq)
	doneCh := make(chan struct{}, len(tasks))
	reports := make([]string, len(tasks))
	for i := range tasks {
		go func(i int) {
			defer func() { doneCh <- struct{}{} }()
			run := func(code string) (string, error) {
				r := codeReq{code: code, resp: make(chan codeRes, 1)}
				select {
				case reqCh <- r:
				case <-w.ctx.Done():
					return "", w.ctx.Err()
				}
				res := <-r.resp
				return res.out, res.err
			}
			reply, err := w.runChild(tasks[i], ids[i], run, true)
			if err != nil {
				reports[i] = fmt.Sprintf("[subagent %d · task: %s]\nStatus: failed\n%v", ids[i], oneLine(tasks[i], 80), err)
				return
			}
			reports[i] = fmt.Sprintf("[subagent %d · task: %s]\n%s", ids[i], oneLine(tasks[i], 80), reply)
		}(i)
	}
	// Serve the children's blocks until every one of them has finished.
	for left := len(tasks); left > 0; {
		select {
		case r := <-reqCh:
			out, err := w.code.Run(r.code)
			r.resp <- codeRes{out: out, err: err}
		case <-doneCh:
			left--
		}
	}
	return reports, nil
}

// runChild drives one child agent run: fresh context seeded with the
// task, up to maxSteps llm steps, js blocks executed via codemode. The
// final plain-text reply (no js block) is the result.
func (w *Workers) runChild(task string, id int, run func(string) (string, error), announced bool) (string, error) {
	// note mirrors child activity: a "sub:<kind>" session-history entry
	// (when history is mounted) and a "loop/event" with the same kind,
	// both carrying the worker number.
	note := func(kind, text string, extra map[string]any) {
		data := map[string]any{"text": text, "worker": id}
		maps.Copy(data, extra)
		if w.hist != nil {
			w.hist.Append("sub:"+kind, data)
		}
		delete(data, "text")
		w.emit("sub:"+kind, text, data)
	}
	// The start event carries the task: the UI's card for this worker.
	// spawnAll announces its whole batch up front so the cards appear in
	// task order rather than in whichever order the children got going.
	if !announced {
		note("start", task, nil)
	}
	steps := 0

	// The sections are read per run, like the parent's per turn: a plugin
	// mounted since (mcp catalog, skills) is documented to the child too.
	var secText string
	if w.secs != nil {
		// Without "workers" the child would read the spawn advert and
		// try to delegate, which depth 1 refuses.
		secText = w.secs.TextExcept("workers")
	}
	system := systemFor(loop.SystemPrompt, secText)
	msgs := []llm.Message{{Role: "user", Content: task}}
	ranClean := false // did any block of this child's work actually succeed?
	for step := 0; step < w.maxSteps; step++ {
		steps++
		reply, err := w.llm.Complete(w.ctx, system, msgs)
		if err != nil {
			note("error", err.Error(), nil)
			note("done", "", map[string]any{"status": "error", "steps": steps})
			return "", fmt.Errorf("workers: subagent llm: %w", err)
		}
		note("assistant", reply, nil)
		msgs = append(msgs, llm.Message{Role: "assistant", Content: reply})
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			status := reportStatus(reply)
			if status == "" {
				status = "ok" // no contract line: the reply is the report
			}
			// A child whose every command failed does not get to say ok,
			// whatever its report claims: the parent summarised one of
			// those once and had to retract it.
			if steps > 1 && !ranClean {
				status = "failed"
			}
			note("done", "", map[string]any{"status": status, "steps": steps})
			return reply, nil
		}
		for _, m := range blocks {
			code := m[1]
			note("code", code, nil)
			out, runErr := run(code)
			if runErr != nil {
				if out = strings.TrimRight(out, "\n"); out != "" {
					out += "\n"
				}
				out += "error: " + runErr.Error()
				out = truncate(out, maxResultBytes)
				note("error", out, nil)
			} else {
				out = truncate(noneNoted(out), maxResultBytes)
				ranClean = true
				note("result", out, nil)
			}
			msgs = append(msgs, llm.Message{Role: "user", Content: "[tool output]\n" + out})
		}
	}
	err := fmt.Errorf("workers: subagent gave up after %d steps", w.maxSteps)
	note("error", err.Error(), nil)
	note("done", "", map[string]any{"status": "error", "steps": steps})
	return "", err
}

// noneNoted names an empty result; see the loop's copy for why.
func noneNoted(out string) string {
	if strings.TrimSpace(out) == "" {
		return "(the block ran and printed nothing — console.log a value to see it)"
	}
	return out
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	// Never cut inside a multi-byte rune: the tail would be invalid
	// UTF-8 for the model and for anything streaming our output.
	for n > 0 && !utf8.RuneStart(s[n]) {
		n--
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

	w.emit = func(kind, text string, data map[string]any) {
		kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text, Data: data})
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
	cm.RegisterTool("spawnAll", w.spawnAll)
	// Optional seam: the loop's prompt-sections registry, so the model
	// learns tools.spawn exists. Withdrawn on unmount.
	if s, err := kernel.Get[sections](kctx, "prompt-sections"); err == nil {
		w.secs = s
		s.Set("workers", fmt.Sprintf(promptSection, maxSpawns, maxSteps))
		kctx.Effect(func() { s.Set("workers", "") })
	}
	return nil
}
