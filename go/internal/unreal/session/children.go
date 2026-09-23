//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"
	"sync/atomic"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/coordinator"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/ops"
	"github.com/andreylukin/bough/internal/unreal/project"
	"github.com/andreylukin/bough/internal/unreal/prompt"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

// childDenied are the tools a subagent never gets by default: depth 1,
// as workers enforce today, and nobody to answer an ask.
var childDenied = []string{"spawn", "agent", "stop_agent", "ask", "secret"}

var workerSeq atomic.Int64

type children struct{ r *Runtime }

// Run runs one subagent on a child coordinator of its own (§12.4): its
// own store file, operation manager, bough.call handler and Gate, an
// adapter from the same llm row, and a projector that writes its rows
// into this session's history as sub:* entries. It runs on the caller's
// goroutine — the parent's foreground spawn call — so parallel spawn
// calls fan out.
func (c children) Run(ctx context.Context, req ChildRequest) (ChildResult, error) {
	r := c.r
	n := workerSeq.Add(1)
	worker := req.Worker
	if worker == "" {
		worker = fmt.Sprint(n)
	}
	sid := session.ID(SID(fmt.Sprintf("%s-w%s-%d", r.sid, worker, n)))
	store, err := localfile.New(r.d.Store)
	if err != nil {
		return ChildResult{Status: "error"}, fmt.Errorf("engine-unreal: child store: %w", err)
	}
	proj := project.New(project.Config{
		Prefix: "sub:", Worker: worker, Detail: r.detail, Render: renderCall,
		JS: r.jsTool(), RowOutput: r.cfg.RowOutput,
	})
	if r.d.Projector != nil {
		proj = r.d.Projector("sub:", worker)
	}
	note := func(kind, text string, extra map[string]any) {
		data := map[string]any{"text": text, "worker": workerValue(worker)}
		for k, v := range extra {
			data[k] = v
		}
		r.d.History.Append("sub:"+kind, data)
		// The live event carries the text beside the data, not in it; a
		// store that keeps the map by reference must not lose the text.
		live := make(map[string]any, len(data))
		for k, v := range data {
			if k != "text" {
				live[k] = v
			}
		}
		r.d.Emit("sub:"+kind, text, live)
	}
	note("start", req.Task, nil)

	// The child's run ctx is its own, not ctx: a cancel stops it with
	// StopHard, which cancels its calls and lets it record them, where
	// cancelling Run's ctx would abandon them mid-flight.
	runCtx, cancelRun := context.WithCancel(r.ctx)
	defer cancelRun()
	handler := r.newHandler(worker, nil)
	defer handler.Close()
	om := ops.New(runCtx, operation.NewLocalOperationManager(runCtx, handler), nil)

	q := newFIFO()
	gate := newGate(r, worker, nil)
	gate.maxSteps = req.MaxSteps
	metas := map[string]project.Meta{}
	stopped := ""
	gate.meta = func(m project.Meta) { q.push(func() { metas[m.ResponseID] = m; proj.Meta(m) }) }
	gate.budget = func(string) { q.push(func() { stopped = "budget" }) }
	defer gate.close()

	obs := store.AddObserver(func(id session.ID, it sessionstore.Item) {
		if id == sid {
			q.push(func() { childItem(it, proj, metas, r, note) })
		}
	})
	defer store.RemoveObserver(obs)

	if _, err := store.Create(runCtx, sid); err != nil {
		note("error", err.Error(), nil)
		note("done", "", map[string]any{"status": "error", "steps": 0})
		return ChildResult{Status: "error", Reply: err.Error()}, nil
	}
	restored, err := store.Resume(runCtx, sid)
	if err != nil {
		note("error", err.Error(), nil)
		note("done", "", map[string]any{"status": "error", "steps": 0})
		return ChildResult{Status: "error", Reply: err.Error()}, nil
	}

	tools := childTools(r.snapshot(), req.Allow)
	reg := toolreg.New(toolreg.Config{Tools: tools, ViewImage: r.viewImage(r.d.Cwd), MaxOutput: r.cfg.MaxOutput})
	b := prompt.Wrap(contextbuilder.NewBuilder(), childSystem(r, req.System), prompt.Placeholder)
	model, _ := r.gate.Model()
	b.SetModel(ullm.Model{ID: orDefault(model, "engine")})
	for _, def := range reg.StaticDefinitions() {
		b.AddTool(def.Tool)
	}
	in, err := inbox.New(runCtx, restored.ExternalInputIDs)
	if err != nil {
		return ChildResult{Status: "error"}, fmt.Errorf("engine-unreal: child inbox: %w", err)
	}
	co := coordinator.New(coordinator.Dependencies{
		SessionID: sid, Inbox: in, Restored: restored, Sessions: store,
		ContextBuilder: b, LLM: gate, Tools: reg, Operations: om,
	})
	exited := make(chan error, 1)
	go func() { exited <- co.Run(runCtx) }()

	task, _ := json.Marshal(req.Task)
	_ = in.Submit(runCtx, inbox.Input{ID: inbox.ID(newInputID()), Kind: inbox.InputExternal, Payload: jsontext.Value(task)})
	_ = in.Submit(runCtx, control(inbox.StopWhenIdle, "subagent finished"))

	status := "done"
	var runErr error
	hard := false
	stopHard := func(why string) {
		if !hard {
			hard = true
			_ = in.Submit(runCtx, control(inbox.StopHard, why))
		}
	}
	cancelled := ctx.Done()
loop:
	for {
		if fs, ok := q.popNow(); ok {
			for _, f := range fs {
				f()
			}
			if stopped == "budget" && status == "done" {
				status = "budget"
				stopHard("step budget reached")
			}
			continue
		}
		select {
		case runErr = <-exited:
			break loop
		case <-cancelled:
			status = "cancelled"
			stopHard("cancelled")
			cancelled = nil
		case <-q.sig:
		}
	}
	// What the observer queued before Run returned is still the child's.
	if fs, ok := q.popNow(); ok {
		for _, f := range fs {
			f()
		}
	}
	steps := gate.Steps()
	reply := lastAssistant(store, runCtx, sid)
	if runErr != nil && status == "done" {
		status = "error"
		reply = runErr.Error()
		note("error", runErr.Error(), nil)
	}
	// The card reads its failure off a sub:error: a child stopped at its
	// budget had none, so it read "Failure details not recorded".
	if status == "budget" {
		note("error", fmt.Sprintf("subagent gave up after %d steps without a report", steps), nil)
	}
	note("done", "", map[string]any{"status": cardStatus(status, reply), "steps": steps})
	return ChildResult{Reply: reply, Status: status, Steps: steps}, nil
}

// cardStatus is a child's end in the words the loop's sub:done uses and
// the TUI and web cards read: ok or failed by the report's own Status
// line, error for a step budget (the loop's "gave up"), cancelled as is.
// "done" was none of them, so a child that finished read as an error.
func cardStatus(status, reply string) string {
	switch status {
	case "done":
		for ln := range strings.SplitSeq(reply, "\n") {
			ln = strings.ToLower(strings.TrimSpace(strings.TrimLeft(ln, "*#- ")))
			if v, ok := strings.CutPrefix(ln, "status:"); ok {
				if strings.HasPrefix(strings.TrimSpace(strings.Trim(v, "*` ")), "fail") {
					return "failed"
				}
				break
			}
		}
		return "ok"
	case "budget":
		return "error"
	}
	return status
}

func control(mode inbox.ControlMode, reason string) inbox.Input {
	b, _ := json.Marshal(inbox.ControlMessage{Mode: mode, Reason: reason})
	return inbox.Input{ID: inbox.ID(newInputID()), Kind: inbox.InputControl, Payload: jsontext.Value(b)}
}

func childItem(it sessionstore.Item, proj *project.Projector, metas map[string]project.Meta, r *Runtime, note func(kind, text string, extra map[string]any)) {
	if it.Kind == sessionstore.ItemModelResponse {
		mr, _ := it.Data.(sessionstore.ModelResponse)
		if m, ok := metas[mr.Response.ID]; ok && m.Err != "" {
			delete(metas, mr.Response.ID)
			note("error", m.Err, nil)
		}
	}
	for _, o := range proj.Item(it) {
		if o.Record {
			r.d.History.Append(o.Kind, o.Data)
		}
		r.d.Emit(o.Kind, o.Text, o.Data)
	}
}

func workerValue(w string) any {
	var n int
	if _, err := fmt.Sscan(w, &n); err == nil && fmt.Sprint(n) == w {
		return n
	}
	return w
}

func childTools(all []agenttools.Tool, allow []string) []agenttools.Tool {
	var out []agenttools.Tool
	for _, t := range all {
		if allow != nil {
			if slices.Contains(allow, t.Name) {
				out = append(out, t)
			}
			continue
		}
		if !slices.Contains(childDenied, t.Name) {
			out = append(out, t)
		}
	}
	return out
}

// childSystem is the parent's prompt without the workers section (a
// child told how to spawn would try, and depth 1 refuses), plus the
// worker's own text.
func childSystem(r *Runtime, extra string) string {
	var p prompt.Parts
	if r.d.Prompt != nil {
		p = r.d.Prompt()
	}
	p.Preamble = r.cfg.SystemPrompt
	if p.Preamble == "" {
		p.Preamble = prompt.Preamble(0)
	}
	p.Guidance = r.cfg.TaskGuidance
	p.Sections = slices.DeleteFunc(p.Sections, func(s prompt.Part) bool { return s.Name == "workers" })
	sys := prompt.Compose(p)
	if strings.TrimSpace(extra) != "" {
		sys += "\n\n" + extra
	}
	return sys
}

func lastAssistant(store *localfile.Store, ctx context.Context, sid session.ID) string {
	reply := ""
	cur := sessionstore.BeforeFirst
	for {
		page, err := store.Items(ctx, sid, cur, 256)
		if err != nil {
			return reply
		}
		for _, it := range page.Items {
			mr, ok := it.Data.(sessionstore.ModelResponse)
			if !ok {
				continue
			}
			for _, o := range mr.Response.Output {
				if m, ok := o.Data.(ullm.Message); ok && m.Role == ullm.RoleAssistant && strings.TrimSpace(m.Text) != "" {
					reply = m.Text
				}
			}
		}
		if !page.More || page.NextAfter <= cur {
			return reply
		}
		cur = page.NextAfter
	}
}

// popNow takes whatever is queued without waiting.
func (f *fifo) popNow() ([]func(), bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.q) == 0 {
		return nil, false
	}
	q := f.q
	f.q = nil
	return q, true
}
