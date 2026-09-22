// Package agenttools is the tool vocabulary shared by the tool rows and
// any engine that exposes them as native tool calls. A tool registered
// here is the same Go function its row already binds into codemode, so
// the loop and the engine run one implementation.
//
// stub.go is B0's copy of go/docs/unreal-engine.md §4.1, written by W6
// so the agent-tools row and main.go's wiring build before W3 lands. W3
// owns this package: delete this file when W3's agenttools.go arrives,
// since both declare the same frozen names.
package agenttools

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"sync"
)

// Tool is one native tool. Name is what the model calls; both Anthropic
// and OpenAI reject names outside ^[a-zA-Z0-9_-]{1,64}$.
type Tool struct {
	Name        string
	Description string
	Schema      map[string]any // JSON Schema, "type": "object"
	// Blocking marks a call whose wait is on the user (ask, secret): it
	// holds its turn with no settle, as tools.ask does today.
	Blocking bool
	// Detail is the call row's text: bash's first command line, the path
	// for write/patch/view, "path:start-end" for a ranged view.
	Detail func(args json.RawMessage) string
	Call   func(ctx context.Context, c Call) (Result, error)
}

type Call struct {
	ID       string          // provider call id; the call row's id
	Args     json.RawMessage // verbatim model arguments (a JSON object)
	Session  string          // bough session id
	Worker   string          // "" for the main agent, else the subagent's name
	Progress func(text string)
}

// Emit sends live output for the call row (a call-delta); nil-safe.
func (c Call) Emit(text string) {
	if c.Progress != nil {
		c.Progress(text)
	}
}

// Result is what the model reads plus what the call row shows. A
// non-empty Error (or a returned error) fails the call: the model reads
// "Error: "+Error, then Text when there is any.
type Result struct {
	Text  string
	Data  map[string]any // exit, add, del, job, path, cmd: copied onto the call entry
	Error string
}

type Registry interface {
	// Register fails, naming the tool, on an invalid or taken name.
	Register(t Tool) (unregister func(), err error)
	Lookup(name string) (Tool, bool)
	Tools() []Tool            // sorted by Name
	Changed() <-chan struct{} // closed and replaced on every Register/unregister
}

// NewRegistry is a mutex-guarded map; the channel pattern is
// close-and-replace.
func NewRegistry() Registry {
	return &registry{tools: map[string]entry{}, changed: make(chan struct{})}
}

var validName = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)

// ValidName reports whether name is callable on every provider.
func ValidName(name string) bool { return validName.MatchString(name) }

// Hooks is the tool half of the hooks row (implemented by
// internal/unreal/hookbridge). A nil Hooks runs every call unhooked.
type Hooks interface {
	// PreTool may refuse a call (deny != "": the model reads
	// "Error: blocked by hook: "+deny) or replace its arguments (args != nil).
	PreTool(ctx context.Context, tool string, c Call, detail string) (args json.RawMessage, deny string)
	// PostTool may rewrite what the model reads.
	PostTool(ctx context.Context, tool string, c Call, detail string, r Result) Result
}

// entry carries a registration number so a row's late unregister (its
// Effect running after a remount re-registered the name) cannot remove
// the newer registration.
type entry struct {
	tool Tool
	n    int
}

type registry struct {
	mu      sync.Mutex
	tools   map[string]entry
	n       int
	changed chan struct{}
}

func (r *registry) Register(t Tool) (func(), error) {
	if !ValidName(t.Name) {
		return nil, fmt.Errorf("agenttools: tool name %q must match ^[a-zA-Z0-9_-]{1,64}$", t.Name)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, taken := r.tools[t.Name]; taken {
		return nil, fmt.Errorf("agenttools: tool %q is already registered", t.Name)
	}
	r.n++
	n := r.n
	r.tools[t.Name] = entry{tool: t, n: n}
	r.bump()
	return func() {
		r.mu.Lock()
		defer r.mu.Unlock()
		if e, ok := r.tools[t.Name]; ok && e.n == n {
			delete(r.tools, t.Name)
			r.bump()
		}
	}, nil
}

// bump wakes every Changed() waiter; r.mu is held.
func (r *registry) bump() {
	close(r.changed)
	r.changed = make(chan struct{})
}

func (r *registry) Lookup(name string) (Tool, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.tools[name]
	return e.tool, ok
}

func (r *registry) Tools() []Tool {
	r.mu.Lock()
	out := make([]Tool, 0, len(r.tools))
	for _, e := range r.tools {
		out = append(out, e.tool)
	}
	r.mu.Unlock()
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

func (r *registry) Changed() <-chan struct{} {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.changed
}
