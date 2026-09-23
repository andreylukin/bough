//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/project"
	"github.com/andreylukin/bough/plugins/loop"
)

// overflowText is what a context overflow tells the user. Nothing trims
// or compacts: auto-compaction is a standing no.
const overflowText = "the conversation no longer fits the model's context window; start a new session (/new) or fork from an earlier turn (/tree)"

// Gate is the llm.Adapter the coordinator sees (§9.6). A provider error
// returned to the coordinator ends Run, and a cancel has to stop the
// model without ending it, so every outcome the harness must not see as
// an error — a user cancel, a provider failure, an overflow, a spent
// budget — comes back as an empty completed Response, with a Meta that
// tells the actor what really happened.
type Gate struct {
	r      *Runtime
	worker string
	// sink receives live deltas; nil for a child (its reply lands whole).
	sink func(agentllm.Delta)
	// meta receives one Meta per Respond, before Respond returns, so it
	// is queued ahead of the ModelResponse the coordinator records.
	meta func(project.Meta)
	// budget is told when a request is refused for max_steps / max_cost.
	budget func(reason string)
	// reasons is the sync mirror's TurnReasons: what the request
	// being gated answers.
	reasons func() []string

	maxSteps int
	maxCost  float64

	mu       sync.Mutex
	seq      uint64
	parked   bool
	pCalls   map[string]bool
	pInputs  map[string]bool
	inflight context.CancelFunc
	inSeq    uint64
	// starting is a request past the park check that has not yet set
	// inflight (resolve() runs between, and builds an adapter on the
	// first request or after /model). A cancel then has no request to
	// stop, so it latches userStop on this seq for Respond to honour.
	starting  uint64
	userStop  uint64 // the seq Cancel stopped; its partial text is kept
	pSeq      uint64
	pAttempt  int
	partial   strings.Builder
	overflow  string // the model the context overflowed on; sticky for it
	steps     int
	cost0     float64
	src       agentllm.Source
	adapter   agentllm.Adapter
	prov      string
	lastModel string
}

func newGate(r *Runtime, worker string, sink func(agentllm.Delta)) *Gate {
	g := &Gate{r: r, worker: worker, sink: sink, maxSteps: r.cfg.MaxSteps, maxCost: r.cfg.MaxCostUSD,
		pCalls: map[string]bool{}, pInputs: map[string]bool{}}
	if worker == "" {
		g.sink = func(d agentllm.Delta) { r.post(func() { r.a.delta(d) }) }
		g.meta = func(m project.Meta) { r.post(func() { r.a.meta(m) }) }
		g.budget = func(reason string) { r.post(func() { r.a.budgetStop(reason) }) }
		g.reasons = r.sync.turnReasons
	}
	return g
}

// Park mutes every request that only answers the given calls and inputs,
// until the first request that answers anything else. A cancel parks
// what it cancelled: their results must reach the model with the next
// input, not start a request of their own after Esc.
func (g *Gate) Park(calls, inputs []string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.parked = true
	for _, c := range calls {
		g.pCalls[c] = true
	}
	for _, in := range inputs {
		g.pInputs[in] = true
	}
}

// CancelInflight stops the request in flight; the Gate answers it with
// the partial text streamed so far.
func (g *Gate) CancelInflight() {
	g.mu.Lock()
	defer g.mu.Unlock()
	switch {
	case g.inflight != nil:
		g.userStop = g.inSeq
		g.inflight()
	case g.starting != 0:
		g.userStop = g.starting
	}
}

// TurnReset starts the budgets over for a new bough turn.
func (g *Gate) TurnReset() {
	cost := 0.0
	if g.r.d.Usage != nil {
		cost = g.r.d.Usage().Cost
	}
	g.mu.Lock()
	g.steps, g.cost0 = 0, cost
	g.mu.Unlock()
}

// Steps is how many requests reached the provider since TurnReset.
func (g *Gate) Steps() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.steps
}

// Model is the model of the adapter the Gate last used or resolved.
func (g *Gate) Model() (model, provider string) {
	ad, prov, err := g.resolve()
	if err != nil {
		g.mu.Lock()
		defer g.mu.Unlock()
		return g.lastModel, g.prov
	}
	return ad.Model(), prov
}

func (g *Gate) Respond(ctx context.Context, req ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	g.mu.Lock()
	g.seq++
	seq := g.seq
	if g.parked {
		var reasons []string
		if g.reasons != nil {
			reasons = g.reasons()
		}
		if g.covered(reasons) {
			g.mu.Unlock()
			return g.answer(seq, project.Meta{Muted: true}, nil), nil
		}
		g.parked = false
		clear(g.pCalls)
		clear(g.pInputs)
	}
	if g.maxSteps > 0 && g.steps >= g.maxSteps {
		g.mu.Unlock()
		return g.stopBudget(seq, "max_steps"), nil
	}
	if g.maxCost > 0 && g.r.d.Usage != nil && g.r.d.Usage().Cost-g.cost0 >= g.maxCost {
		g.mu.Unlock()
		return g.stopBudget(seq, "max_cost"), nil
	}
	g.steps++
	g.starting = seq
	g.mu.Unlock()
	defer func() {
		g.mu.Lock()
		if g.starting == seq {
			g.starting = 0
		}
		g.mu.Unlock()
	}()

	ad, prov, err := g.resolve()
	if err != nil {
		return g.answer(seq, project.Meta{Err: err.Error()}, nil), nil
	}
	model := ad.Model()
	g.mu.Lock()
	g.lastModel = model
	sticky := g.overflow != "" && g.overflow == model
	g.mu.Unlock()
	if sticky {
		return g.answer(seq, project.Meta{Model: model, Provider: prov, Err: overflowText}, nil), nil
	}

	child, cancel := context.WithCancel(agentllm.WithSeq(ctx, seq))
	defer cancel()
	g.mu.Lock()
	g.inflight, g.inSeq = cancel, seq
	g.starting = 0
	g.pSeq, g.pAttempt = seq, 0
	g.partial.Reset()
	latched := g.userStop == seq
	g.mu.Unlock()
	var resp ullm.Response
	if latched {
		// Esc landed while this request was being set up: it never
		// reaches the provider, so no call it would make runs after Esc.
		cancel()
		err = context.Canceled
	} else {
		if g.sink != nil {
			g.sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaStart})
		}
		resp, err = ad.Respond(child, req, o)
	}

	g.mu.Lock()
	stopped := g.userStop == seq
	if g.inSeq == seq {
		g.inflight = nil
	}
	partial := g.partial.String()
	g.mu.Unlock()

	switch {
	case ctx.Err() != nil:
		// Superseded (a steer, a newer input) or Run is ending: the
		// coordinator drops this result whatever it is.
		return ullm.Response{}, ctx.Err()
	case stopped:
		var out []ullm.Item
		// The cut reply stays in context on purpose: the loop projects
		// one the same way, and the model should know where it was
		// stopped. Thinking from a cut stream has no signature; dropped.
		if strings.TrimSpace(partial) != "" {
			out = []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: partial}}}
		}
		r := ullm.Response{ID: fmt.Sprintf("bough-cancel-%d", seq), Stop: ullm.StopComplete, Output: out}
		g.emitMeta(project.Meta{ResponseID: r.ID, Model: model, Provider: prov, Partial: true})
		return r, nil
	case err != nil:
		text := err.Error()
		if errors.Is(err, agentllm.ErrContextOverflow) {
			g.mu.Lock()
			g.overflow = model
			g.mu.Unlock()
			text = overflowText
		}
		return g.answer(seq, project.Meta{Model: model, Provider: prov, Err: text}, nil), nil
	}
	if resp.ID == "" {
		resp.ID = fmt.Sprintf("bough-%d", seq)
	}
	if resp.Stop == "" {
		resp.Stop = ullm.StopComplete
	}
	// A <system-*> span in the model's own reply is a fabricated system
	// message (glm-5.3-flash forged one telling the agent to delete files
	// and force-push). Stripped here, before the coordinator records the
	// response, it reaches neither history nor the model's next request.
	for i, it := range resp.Output {
		if m, ok := it.Data.(ullm.Message); ok && it.Type == ullm.ItemMessage && m.Role == ullm.RoleAssistant {
			if s := loop.StripFabrications(m.Text); s != m.Text {
				m.Text = s
				resp.Output[i].Data = m
			}
		}
	}
	if served := agentllm.ServedModel(resp.Usage); served != "" {
		model = served
	}
	g.emitMeta(project.Meta{ResponseID: resp.ID, Model: model, Provider: prov})
	return resp, nil
}

// covered: every reason the request answers is one the Gate parked. An
// empty set counts: a request that answers nothing new after a cancel
// has nothing to say to the model.
func (g *Gate) covered(reasons []string) bool {
	for _, r := range reasons {
		kind, id, _ := strings.Cut(r, ":")
		switch kind {
		case "call":
			if !g.pCalls[id] {
				return false
			}
		case "input":
			if !g.pInputs[id] {
				return false
			}
		default:
			return false
		}
	}
	return true
}

func (g *Gate) stopBudget(seq uint64, reason string) ullm.Response {
	r := g.answer(seq, project.Meta{Muted: true}, nil)
	if g.budget != nil {
		g.budget(reason)
	}
	return r
}

// answer is an empty completed response the Gate gives without (or in
// place of) the provider.
func (g *Gate) answer(seq uint64, m project.Meta, out []ullm.Item) ullm.Response {
	prefix := "bough-error-"
	if m.Muted {
		prefix = "bough-muted-"
	}
	r := ullm.Response{ID: fmt.Sprintf("%s%d", prefix, seq), Stop: ullm.StopComplete, Output: out}
	m.ResponseID = r.ID
	g.emitMeta(m)
	return r
}

func (g *Gate) emitMeta(m project.Meta) {
	if g.meta != nil {
		g.meta(m)
	}
}

// onDelta tracks the partial text of the request in flight (what a
// cancel keeps) and forwards the delta.
func (g *Gate) onDelta(d agentllm.Delta) {
	g.mu.Lock()
	if d.Seq == g.pSeq {
		if d.Attempt != g.pAttempt {
			g.pAttempt = d.Attempt
			g.partial.Reset()
		}
		if d.Kind == agentllm.DeltaText {
			g.partial.WriteString(d.Text)
		}
	}
	g.mu.Unlock()
	if g.sink != nil {
		g.sink(d)
	}
}

// resolve returns the adapter for the llm row as it is now. The row is
// read per request: /model and /think change it mid-session without a
// coordinator restart, and a swapped row is a new service value, so a
// new adapter.
func (g *Gate) resolve() (agentllm.Adapter, string, error) {
	if g.r.d.LLM == nil {
		return nil, "", errors.New("engine-unreal: no llm row")
	}
	src, prov, err := g.r.d.LLM()
	if err != nil {
		return nil, "", err
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.adapter != nil && same(g.src, src) {
		return g.adapter, g.prov, nil
	}
	opts := agentllm.Options{Session: g.r.sid, Worker: g.worker, Sink: g.onDelta}
	if g.worker != "" {
		opts.Session = g.r.sid + "-w" + g.worker
		opts.Sink = nil
	}
	if g.r.cfg.Trace {
		opts.Trace = g.r.tracer()
	}
	ad, err := src.AgentAdapter(opts)
	if err != nil {
		return nil, "", fmt.Errorf("engine-unreal: %w", err)
	}
	if g.adapter != nil {
		_ = g.adapter.Close()
	}
	g.src, g.adapter, g.prov = src, ad, prov
	return ad, prov, nil
}

// close releases the adapter.
func (g *Gate) close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.adapter != nil {
		_ = g.adapter.Close()
		g.adapter = nil
	}
}

// same compares two service values without panicking on an
// uncomparable dynamic type (that is a new value every time anyway).
func same(a, b agentllm.Source) (eq bool) {
	defer func() {
		if recover() != nil {
			eq = false
		}
	}()
	return a == b
}

// tracer writes request/response pairs to <store>/trace/<sid>.jsonl,
// redacted again on the way out: an adapter never puts a key in an
// Exchange, but a secret the user pasted can be in a request body.
func (r *Runtime) tracer() func(agentllm.Exchange) {
	dir := filepath.Join(r.d.Store, "trace")
	path := filepath.Join(dir, r.sid+".jsonl")
	var mu sync.Mutex
	return func(x agentllm.Exchange) {
		red := func(b []byte) string {
			s := string(b)
			if r.d.Redact != nil {
				s = r.d.Redact(s)
			}
			return s
		}
		line, err := json.Marshal(map[string]any{
			"at": time.Now().UTC().Format(time.RFC3339Nano), "provider": x.Provider, "attempt": x.Attempt,
			"status": x.Status, "request": red(x.Request), "response": red(x.Response), "error": x.Err,
		})
		if err != nil {
			return
		}
		mu.Lock()
		defer mu.Unlock()
		if os.MkdirAll(dir, 0o700) != nil {
			return
		}
		f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
		if err != nil {
			return
		}
		defer f.Close()
		_, _ = f.Write(append(line, '\n'))
	}
}
