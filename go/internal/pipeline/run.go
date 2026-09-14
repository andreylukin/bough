package pipeline

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/andreylukin/bough/plugins/history"
)

type Options struct {
	Home     string    // explicit; tests use t.TempDir()
	Sessions Sessions  //
	Sets     []string  // extra --set for every child (tests: llm.plugin=llm-echo)
	Out      io.Writer // human progress lines; nil = discard
	// ID is a run id minted by the caller (`loop run --detach` names the
	// run dir before the runner process exists); "" = a new one.
	ID string
}

type State struct {
	ID       string            `json:"id"`
	Name     string            `json:"name"`
	Status   string            `json:"status"`
	Node     string            `json:"node"`
	Step     int               `json:"step"`
	Visits   map[string]int    `json:"visits"`
	Sessions map[string]string `json:"sessions"`
	Pid      int               `json:"pid"`
	Started  time.Time         `json:"started"`
	Ended    time.Time         `json:"ended,omitzero"`
	Reason   string            `json:"reason,omitempty"` // why a run ended in error
}

type Event struct {
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Node string         `json:"node,omitempty"`
	Step int            `json:"step,omitempty"`
	Text string         `json:"text,omitempty"`
	Data map[string]any `json:"data,omitempty"`
}

// Run statuses.
const (
	StatusRunning   = "running"
	StatusPassed    = "passed"
	StatusFailed    = "failed"
	StatusExhausted = "exhausted"
	StatusStopped   = "stopped"
	StatusError     = "error"
)

type Runner struct {
	p       *Pipeline
	opt     Options
	id, dir string
	holdout []string // staged copies

	mu     sync.Mutex // guards st and active
	st     State
	active map[string]int // node -> step of its in-flight visit

	evMu sync.Mutex
}

// result is steps/<step>-<node>/result.json.
type result struct {
	Node    string    `json:"node"`
	Visit   int       `json:"visit"`
	Session string    `json:"session,omitempty"`
	Exit    *int      `json:"exit,omitempty"`
	Verdict string    `json:"verdict,omitempty"`
	Route   string    `json:"route"`
	Reason  string    `json:"reason"`
	Started time.Time `json:"started"`
	Ended   time.Time `json:"ended"`
	step    int
}

func RunsDir(home string) string { return filepath.Join(home, ".bough", "loops") }

// NewRunner mints the run id, creates the run dir, stages the holdout
// and runs its preflight.
func NewRunner(p *Pipeline, opt Options) (r *Runner, err error) {
	if opt.Home == "" {
		return nil, errors.New("pipeline: Options.Home is required")
	}
	if opt.Sessions == nil {
		return nil, errors.New("pipeline: Options.Sessions is required")
	}
	if opt.Out == nil {
		opt.Out = io.Discard
	}
	id := opt.ID
	if id == "" {
		id = history.NewID()
	}
	run := &Runner{p: p, opt: opt, id: id, dir: filepath.Join(RunsDir(opt.Home), id), active: map[string]int{}}
	r = run
	if err := os.MkdirAll(r.dir, 0o700); err != nil {
		return nil, fmt.Errorf("pipeline: run dir: %w", err)
	}
	r.st = State{ID: id, Name: p.Name, Status: StatusRunning, Visits: map[string]int{}, Sessions: map[string]string{}, Pid: os.Getpid(), Started: time.Now()}
	// run, not r: `return nil, err` has already set r to nil by now.
	defer func() {
		if err != nil {
			run.Fail(err.Error())
		}
	}()
	raw := p.raw
	if raw == nil {
		b, err := yaml.Marshal(p)
		if err != nil {
			return nil, err
		}
		raw = b
	}
	if err := os.WriteFile(filepath.Join(r.dir, "pipeline.yml"), raw, 0o444); err != nil {
		return nil, fmt.Errorf("pipeline: freeze: %w", err)
	}
	staged, err := stageHoldout(p, r.dir)
	if err != nil {
		return nil, fmt.Errorf("pipeline: %w", err)
	}
	r.holdout = staged
	if err := preflightHoldout(p, staged, opt.Home); err != nil {
		return nil, fmt.Errorf("pipeline: %w", err)
	}
	r.saveState()
	return r, nil
}

// Fail ends a run that never ran in error, so `loop status` shows why.
func (r *Runner) Fail(why string) {
	r.mu.Lock()
	r.st.Status, r.st.Reason, r.st.Ended = StatusError, why, time.Now()
	r.mu.Unlock()
	r.saveState()
	r.event(Event{Kind: "end", Text: StatusError, Data: map[string]any{"reason": why}})
}

func (r *Runner) ID() string  { return r.id }
func (r *Runner) Dir() string { return r.dir }

// Run walks the graph until done, fail, exhaustion or ctx cancel, which
// ends the run stopped and kills the live node children.
func (r *Runner) Run(ctx context.Context) (State, error) {
	r.event(Event{Kind: "start", Text: r.p.Name, Data: map[string]any{"goal": r.p.Goal}})
	cctx, stopCoaches := context.WithCancel(ctx)
	var wg sync.WaitGroup
	for _, c := range r.p.Coaches {
		wg.Go(func() {
			r.coach(cctx, c, func() (string, bool) {
				r.mu.Lock()
				defer r.mu.Unlock()
				return r.st.Sessions[c.Target], r.active[c.Target] > 0
			})
		})
	}

	node, input := r.p.Start, ""
	status, why := "", ""
	for status == "" {
		switch {
		case ctx.Err() != nil:
			status, why = StatusStopped, "stopped"
			continue
		case node == targetDone:
			status, why = StatusPassed, "done"
			continue
		case node == targetFail:
			status, why = StatusFailed, "fail"
			continue
		}
		n := r.p.Nodes[node]
		// Caps are checked before counting, so state names the last step
		// that ran.
		r.mu.Lock()
		step, visit := r.st.Step+1, r.st.Visits[node]+1
		r.mu.Unlock()
		if r.p.MaxSteps > 0 && step > r.p.MaxSteps {
			status, why = StatusExhausted, fmt.Sprintf("max_steps %d", r.p.MaxSteps)
			continue
		}
		if visit > n.MaxVisits {
			status, why = StatusExhausted, fmt.Sprintf("%s: max_visits %d", node, n.MaxVisits)
			continue
		}
		r.mu.Lock()
		r.st.Step, r.st.Visits[node], r.st.Node = step, visit, node
		r.mu.Unlock()
		r.saveState()
		r.event(Event{Kind: "visit", Node: node, Step: step, Data: map[string]any{"visit": visit}})
		fmt.Fprintf(r.opt.Out, "step %d: %s (visit %d)\n", step, node, visit)

		sdir := filepath.Join(r.dir, "steps", fmt.Sprintf("%d-%s", step, node))
		if err := os.MkdirAll(sdir, 0o755); err != nil {
			status, why = StatusError, err.Error()
			continue
		}
		res := result{Node: node, Visit: visit, Started: time.Now(), step: step}
		var routed string
		if n.Type == "check" {
			routed = r.check(ctx, n, sdir, &res)
		} else {
			routed = r.agent(ctx, n, r.expand(n, input, visit), sdir, &res)
		}
		res.Ended = time.Now()
		if b, err := json.MarshalIndent(res, "", "  "); err == nil {
			_ = os.WriteFile(filepath.Join(sdir, "result.json"), append(b, '\n'), 0o644)
		}
		if ctx.Err() != nil {
			continue // stopped: no route
		}
		r.event(Event{Kind: "route", Node: node, Step: step, Text: res.Route, Data: map[string]any{"reason": res.Reason}})
		fmt.Fprintf(r.opt.Out, "  -> %s (%s)\n", res.Route, res.Reason)
		node, input = res.Route, routed
		r.saveState()
	}
	stopCoaches()
	wg.Wait()

	r.mu.Lock()
	ids := make([]string, 0, len(r.st.Sessions))
	for _, id := range r.st.Sessions {
		ids = append(ids, id)
	}
	r.mu.Unlock()
	for _, id := range ids {
		if r.opt.Sessions.Live(id) {
			_ = r.opt.Sessions.Kill(id)
		}
	}

	r.mu.Lock()
	r.st.Status, r.st.Ended = status, time.Now()
	if status == StatusError {
		r.st.Reason = why
	}
	st := r.st
	r.mu.Unlock()
	r.saveState()
	r.event(Event{Kind: "end", Node: st.Node, Step: st.Step, Text: status, Data: map[string]any{"reason": why}})
	fmt.Fprintf(r.opt.Out, "%s: %s\n", status, why)
	if status == StatusError {
		return copyState(st), errors.New(why)
	}
	return copyState(st), nil
}

// expand fills the template variables by plain replacement.
func (r *Runner) expand(n *Node, input string, visit int) string {
	holdoutDir, runDir := "", ""
	if len(n.Holdout) > 0 {
		holdoutDir = filepath.Join(r.dir, "holdout")
	}
	if n.Mode != "project" {
		runDir = r.dir
	}
	return strings.NewReplacer(
		"{{goal}}", r.p.Goal,
		"{{input}}", input,
		"{{holdout_dir}}", holdoutDir,
		"{{visit}}", strconv.Itoa(visit),
		"{{run_dir}}", runDir,
	).Replace(n.Prompt)
}

func (r *Runner) setInflight(node string, step int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.active[node] = step
}

func (r *Runner) inflight(node string) int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.active[node]
}

func copyState(s State) State {
	s.Visits = maps(s.Visits)
	s.Sessions = maps(s.Sessions)
	return s
}

func maps[V any](m map[string]V) map[string]V {
	out := make(map[string]V, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}

// saveState writes state.json atomically (tmp + rename).
func (r *Runner) saveState() {
	r.mu.Lock()
	b, err := json.MarshalIndent(r.st, "", "  ")
	r.mu.Unlock()
	if err != nil {
		return
	}
	path := filepath.Join(r.dir, "state.json")
	tmp := path + ".tmp"
	if os.WriteFile(tmp, append(b, '\n'), 0o644) == nil {
		_ = os.Rename(tmp, path)
	}
}

// event appends one line to events.jsonl.
func (r *Runner) event(e Event) {
	e.At = time.Now()
	b, err := json.Marshal(e)
	if err != nil {
		return
	}
	r.evMu.Lock()
	defer r.evMu.Unlock()
	f, err := os.OpenFile(filepath.Join(r.dir, "events.jsonl"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	_, _ = f.Write(append(b, '\n'))
}

func ReadState(home, id string) (State, error) {
	var s State
	b, err := os.ReadFile(filepath.Join(RunsDir(home), id, "state.json"))
	if errors.Is(err, os.ErrNotExist) {
		return s, fmt.Errorf("no loop run %s", id)
	}
	if err != nil {
		return s, err
	}
	err = json.Unmarshal(b, &s)
	return s, err
}

// ReadEvents is a run's events.jsonl, oldest first.
func ReadEvents(home, id string) ([]Event, error) {
	b, err := os.ReadFile(filepath.Join(RunsDir(home), id, "events.jsonl"))
	if err != nil {
		return nil, err
	}
	var out []Event
	for line := range strings.SplitSeq(strings.TrimSpace(string(b)), "\n") {
		var e Event
		if json.Unmarshal([]byte(line), &e) == nil {
			out = append(out, e)
		}
	}
	return out, nil
}

// ListRuns is every run with a readable state, newest first.
func ListRuns(home string) ([]State, error) {
	dirs, err := os.ReadDir(RunsDir(home))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []State
	for _, d := range dirs {
		if s, err := ReadState(home, d.Name()); err == nil {
			out = append(out, s)
		}
	}
	slices.SortFunc(out, func(a, b State) int { return b.Started.Compare(a.Started) })
	return out, nil
}

func sortedNames(p *Pipeline) []string {
	names := make([]string, 0, len(p.Nodes))
	for n := range p.Nodes {
		names = append(names, n)
	}
	slices.Sort(names)
	return names
}

func itoa(n int) string { return strconv.Itoa(n) }
