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

	"github.com/andreylukin/bough/internal/schema"
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
const SubSystemPrompt = `You are a bough subagent spawned for ONE task. Complete exactly that task with the tools above (you cannot spawn), then end with your REPORT inside a ` + "```stop" + ` block — that is what hands your work back. The report is all the parent agent sees, so make it self-contained and short (under 30 lines):

Status: ok | failed
Findings: what you established, as bullets (facts, numbers, paths)
Files: paths you changed, or "none"
Open: questions or blockers for the parent, or "none"`

// schemaSection and schemaNote are the loop's, so a child follows the
// same structured-answer contract its parent does.
const schemaSection = loop.SchemaSection
const schemaNote = loop.SchemaNote

// promptSection documents tools.spawn to the parent model (registered
// into the loop's "prompt-sections" service when present).
const promptSection = `Subagents — when to delegate:
- Exploring a codebase to answer a broad question ("what is this project", "how does X work", "where is Y handled") is the case delegation exists for: split it into independent areas and send them to tools.spawnAll in ONE call. The children read the files; only their reports reach you, so a wide survey costs you a few hundred lines instead of tens of thousands.
- A needle lookup you can do in one command — a known path, a single grep — is faster done yourself. Do not delegate a shell command.
- Prefer spawnAll over several spawn calls: the children wait on the model in parallel, so N tasks take about as long as the slowest one.
- Give each child one self-contained brief: what to find out, where to look, and what to report. It cannot see this conversation and cannot spawn.
- Pass a JSON Schema as a second argument — tools.spawn(task, schema) or tools.spawnAll(tasks, schema) — when you want a VALUE rather than a paragraph: the child's report must then match it, is checked before it counts as finished, and comes back as a parsed object your program can index. Use it whenever you are going to pick fields out of the reply anyway.
Both calls are synchronous — no await. Limits: at most %d spawns per turn and %d steps per child, so scope each child's task to fit and do small things yourself.`

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
const maxResultBytes = 64 * 1024

// jsBlock is a local copy of the loop plugin's fence matcher (unexported
// there).
var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// Codemode is the slice of the "codemode" service workers needs.
type Codemode interface {
	RegisterTool(name string, fn any)
	Run(code string) (string, error)
}

// ctxCodemode is codemode's optional context-aware run: a child's
// block executes under the TURN's context, so tools.bash inside a
// subagent dies with the turn like the parent's own does.
type ctxCodemode interface {
	RunCtx(ctx context.Context, code string) (string, error)
}

// runContexter exposes the context of the block in flight — the
// parent's turn context, since spawn is only ever called from one.
type runContexter interface {
	RunContext() context.Context
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
	ctx       context.Context                              // the plugin's: outlives any one turn
	turn      func() context.Context                       // the running block's turn context
	maxSpawns int
	maxSteps  int
	spawns    int  // spawns this parent turn; reset on the loop's "done"
	inChild   bool // a child run is active: no nested spawns
	nextID    int  // worker numbering, monotonic per session
}

// spawn is tools.spawn(task) -> final reply. A returned error becomes a
// JS exception in the calling code block.
func (w *Workers) spawn(task string, shape ...map[string]any) (any, error) {
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
	tctx := w.turnCtx()
	run := func(code string) (string, error) { return w.runBlock(tctx, code) }
	sch := shapeOf(shape)
	reply, err := w.runChild(tctx, task, id, run, false, sch)
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
	// With a schema the caller wanted a value, not a paragraph: hand
	// back the parsed object so the parent's program can use it.
	if len(sch) > 0 {
		if v, issues := sch.ValidateJSON(reply); len(issues) == 0 {
			return v, nil
		}
	}
	// Provenance: the parent (and the user reading its code output)
	// can tell delegated findings from the parent's own work.
	return fmt.Sprintf("[subagent %d · task: %s]\n%s", id, oneLine(task, 80), reply), nil
}

// shapeOf reads the optional schema argument of spawn/spawnAll.
func shapeOf(shape []map[string]any) schema.Schema {
	if len(shape) == 0 || len(shape[0]) == 0 {
		return nil
	}
	return schema.Schema(shape[0])
}

// turnCtx is the context a spawned child lives under: the parent
// turn's, so pressing esc kills the children with it. Background jobs
// deliberately do NOT hang off this — they outlive the turn. Falls
// back to the plugin's context when there is no block in flight.
func (w *Workers) turnCtx() context.Context {
	if w.turn != nil {
		if c := w.turn(); c != nil {
			return c
		}
	}
	return w.ctx
}

// runBlock executes a child's code block under ctx.
func (w *Workers) runBlock(ctx context.Context, code string) (string, error) {
	if cr, ok := w.code.(ctxCodemode); ok {
		return cr.RunCtx(ctx, code)
	}
	return w.code.Run(code)
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
func (w *Workers) spawnAll(tasks []string, shape ...map[string]any) ([]any, error) {
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

	tctx := w.turnCtx()
	reqCh := make(chan codeReq)
	doneCh := make(chan struct{}, len(tasks))
	sch := shapeOf(shape)
	reports := make([]any, len(tasks))
	for i := range tasks {
		go func(i int) {
			defer func() { doneCh <- struct{}{} }()
			run := func(code string) (string, error) {
				r := codeReq{code: code, resp: make(chan codeRes, 1)}
				select {
				case reqCh <- r:
				case <-tctx.Done():
					return "", tctx.Err()
				}
				res := <-r.resp
				return res.out, res.err
			}
			reply, err := w.runChild(tctx, tasks[i], ids[i], run, true, sch)
			if err != nil {
				reports[i] = fmt.Sprintf("[subagent %d · task: %s]\nStatus: failed\n%v", ids[i], oneLine(tasks[i], 80), err)
				return
			}
			if len(sch) > 0 {
				if v, issues := sch.ValidateJSON(reply); len(issues) == 0 {
					reports[i] = v
					return
				}
			}
			reports[i] = fmt.Sprintf("[subagent %d · task: %s]\n%s", ids[i], oneLine(tasks[i], 80), reply)
		}(i)
	}
	// Serve the children's blocks until every one of them has finished.
	for left := len(tasks); left > 0; {
		select {
		case r := <-reqCh:
			// After a cancel every pending block fails at once: the
			// children then unwind on their own, so the serve loop
			// still drains to zero instead of leaking goroutines.
			if err := tctx.Err(); err != nil {
				r.resp <- codeRes{err: err}
				continue
			}
			out, err := w.runBlock(tctx, r.code)
			r.resp <- codeRes{out: out, err: err}
		case <-doneCh:
			left--
		}
	}
	if err := tctx.Err(); err != nil {
		return nil, fmt.Errorf("workers: cancelled: %w", err)
	}
	return reports, nil
}

// runChild drives one child agent run: fresh context seeded with the
// task, up to maxSteps llm steps, js blocks executed via codemode. The
// final plain-text reply (no js block) is the result.
func (w *Workers) runChild(ctx context.Context, task string, id int, run func(string) (string, error), announced bool, sch schema.Schema) (string, error) {
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
	if len(sch) > 0 {
		system += "\n\n" + fmt.Sprintf(schemaSection, sch.Describe())
	}
	msgs := []llm.Message{{Role: "user", Content: task}}
	ranClean := false // did any block of this child's work actually succeed?
	for step := 0; step < w.maxSteps; step++ {
		steps++
		if err := ctx.Err(); err != nil {
			note("done", "", map[string]any{"status": "cancelled", "steps": steps})
			return "", fmt.Errorf("workers: cancelled: %w", err)
		}
		reply, err := w.llm.Complete(ctx, system, msgs)
		if err != nil {
			note("error", err.Error(), nil)
			note("done", "", map[string]any{"status": "error", "steps": steps})
			return "", fmt.Errorf("workers: subagent llm: %w", err)
		}
		// A child's reply reaches the parent as tool output: a
		// fabricated system message in it would read there as an
		// instruction from the harness.
		reply = loop.StripFabrications(reply)
		// A child ends its run the way the parent does: the report
		// lives in a stop block, unwrapped before it is shown or
		// handed back. A reply with no block at all is still taken as
		// the report — a child has a step budget, and an unbounded
		// push-back loop inside a subagent is worse than a slightly
		// informal report.
		stopped := false
		if answer, ok := loop.StopAnswer(reply); ok {
			reply, stopped = answer, true
		}
		dropped := 0
		if !stopped {
			// One block per step, like the parent: a child that writes
			// a dozen blind commands in one reply is the same failure.
			reply, dropped = loop.FirstBlockOnly(reply)
		}
		note("assistant", reply, nil)
		msgs = append(msgs, llm.Message{Role: "assistant", Content: reply})
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			// A structured report is checked before it counts as
			// finished: the mismatches go back as the child's next
			// message, inside its own step budget.
			if len(sch) > 0 {
				if _, issues := sch.ValidateJSON(reply); len(issues) > 0 {
					note("error", "report does not match the schema:\n- "+strings.Join(issues, "\n- "), nil)
					msgs = append(msgs, llm.Message{Role: "user",
						Content: fmt.Sprintf(schemaNote, "- "+strings.Join(issues, "\n- "))})
					continue
				}
			}
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
			if dropped > 0 {
				out += fmt.Sprintf("\n\n[only the first of your %d code blocks ran. Write ONE block per reply, read its output, then decide the next one.]", dropped+1)
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
	// Keep the head AND the tail. A tail-only cut threw away three of
	// six subagent reports and left the third mid-sentence; a report's
	// conclusion lives at its end, so both ends are worth more than the
	// middle. Never cut inside a multi-byte rune.
	head, tail := n*2/3, n-n*2/3
	for head > 0 && !utf8.RuneStart(s[head]) {
		head--
	}
	for tail > 0 && !utf8.RuneStart(s[len(s)-tail]) {
		tail--
	}
	return fmt.Sprintf("%s\n… [%d bytes cut] …\n%s", s[:head], len(s)-head-tail, s[len(s)-tail:])
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
	if rc, ok := cm.(runContexter); ok {
		w.turn = rc.RunContext
	}
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
	if d, ok := cm.(interface{ Describe(name, line string) }); ok {
		d.Describe("spawn", `tools.spawn(task) -> string: run ONE bounded child agent (same tools, fresh context, no nested spawns) and get its report.`)
		d.Describe("spawnAll", `tools.spawnAll([task, …]) -> [report, …]: run several children AT ONCE; N tasks take about as long as the slowest.`)
	}
	// Optional seam: the loop's prompt-sections registry, so the model
	// learns tools.spawn exists. Withdrawn on unmount.
	if s, err := kernel.Get[sections](kctx, "prompt-sections"); err == nil {
		w.secs = s
		s.Set("workers", fmt.Sprintf(promptSection, maxSpawns, maxSteps))
		kctx.Effect(func() { s.Set("workers", "") })
	}
	return nil
}
