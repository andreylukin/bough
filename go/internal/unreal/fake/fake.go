//go:build !windows

// Package fake is a scripted harness adapter: a FIFO tape of steps,
// each the response to one model request. Engine tests drive a real
// coordinator with it, and the llm-script row links it into the binary
// so headless e2e runs and the web spec get a deterministic model.
//
// Nothing here touches the network or a clock except Hold and HoldMS,
// and an exhausted or mismatched tape answers with text instead of
// hanging, so a broken script fails a test instead of timing it out.
package fake

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"strings"
	"sync"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

// Step is the response to one request.
type Step struct {
	Match  func(ullm.Request) error // nil = accept; an error fails the test with the rendered request
	Want   string                   // JSON form: substring of the last user-side text
	Output []ullm.Item
	Stop   ullm.StopReason  // "" = complete
	Usage  ullm.Usage       // zero = {InputTokens: 100*len(Input), OutputTokens: 10}
	Deltas []agentllm.Delta // sent through Options.Sink, Seq from ctx, before returning
	Hold   <-chan struct{}  // block until closed or ctx done (steer/cancel tests)
	HoldMS int              // JSON form of Hold
	Err    error            // returned from Respond (the Gate converts it)
}

// Reporter is satisfied by testing.TB; the package does not import
// "testing", because llm-script links it into the binary.
type Reporter interface {
	Helper()
	Errorf(format string, args ...any)
}

// Recorded is one request the adapter saw, deep-copied.
type Recorded struct {
	Request ullm.Request
	Options ullm.RequestOptions
}

// tape is the state every view of one adapter shares: the steps, what
// was asked, and who is waiting for how many requests.
type tape struct {
	mu      sync.Mutex
	r       Reporter
	steps   []Step
	next    int
	reqs    []Recorded
	changed chan struct{} // closed and replaced on every request
}

// Adapter is the scripted adapter. The zero value is not usable; call
// New.
type Adapter struct {
	t *tape

	mu   sync.Mutex
	opts agentllm.Options
}

var _ agentllm.Adapter = (*Adapter)(nil)

// New returns an adapter that answers with steps in order. r == nil
// (llm-script) turns a mismatch or an exhausted tape into a
// "[script: …]" reply instead of a test failure.
func New(r Reporter, steps ...Step) *Adapter {
	return &Adapter{t: &tape{r: r, steps: steps, changed: make(chan struct{})}}
}

// View returns an adapter that consumes the same tape with its own
// options. A parent session and its subagents each get a view, so one
// script plays out across all of them in request order.
func (a *Adapter) View(o agentllm.Options) *Adapter {
	return &Adapter{t: a.t, opts: o}
}

func (a *Adapter) Provider() string { return "fake" }
func (a *Adapter) Model() string    { return "fake-model" }
func (a *Adapter) Close() error     { return nil }

// SetOptions sets the sink and the rest of the options.
func (a *Adapter) SetOptions(o agentllm.Options) {
	a.mu.Lock()
	a.opts = o
	a.mu.Unlock()
}

// Requests returns deep copies of every request made so far.
func (a *Adapter) Requests() []Recorded {
	a.t.mu.Lock()
	defer a.t.mu.Unlock()
	out := make([]Recorded, len(a.t.reqs))
	copy(out, a.t.reqs)
	return out
}

// Wait blocks until n requests were made or ctx ends.
func (a *Adapter) Wait(ctx context.Context, n int) error {
	for {
		a.t.mu.Lock()
		got, ch := len(a.t.reqs), a.t.changed
		a.t.mu.Unlock()
		if got >= n {
			return nil
		}
		select {
		case <-ch:
		case <-ctx.Done():
			return fmt.Errorf("fake: %d of %d requests before %w", got, n, ctx.Err())
		}
	}
}

// Respond answers with the next step.
func (a *Adapter) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	t := a.t
	t.mu.Lock()
	t.reqs = append(t.reqs, Recorded{Request: clone(r), Options: o})
	close(t.changed)
	t.changed = make(chan struct{})
	n := len(t.reqs)
	var step Step
	have := t.next < len(t.steps)
	if have {
		step = t.steps[t.next]
		t.next++
	}
	rep := t.r
	t.mu.Unlock()

	if err := ctx.Err(); err != nil {
		return ullm.Response{}, err
	}
	if !have {
		if rep != nil {
			rep.Helper()
			rep.Errorf("fake: script exhausted at request %d:\n%s", n, Render(r))
		}
		return textResponse(n, "[script exhausted]"), nil
	}
	if msg := mismatch(step, r); msg != "" {
		if rep != nil {
			rep.Helper()
			rep.Errorf("fake: request %d: %s\n%s", n, msg, Render(r))
		}
		return textResponse(n, "[script: "+msg+"]"), nil
	}

	a.mu.Lock()
	sink := a.opts.Sink
	a.mu.Unlock()
	seq := agentllm.SeqOf(ctx)
	for _, d := range step.Deltas {
		if sink == nil {
			break
		}
		d.Seq = seq
		if d.Attempt == 0 {
			d.Attempt = 1
		}
		sink(d)
	}

	hold := step.Hold
	if hold == nil && step.HoldMS > 0 {
		timer := time.NewTimer(time.Duration(step.HoldMS) * time.Millisecond)
		defer timer.Stop()
		hold = timerChan(timer)
	}
	if hold != nil {
		select {
		case <-hold:
		case <-ctx.Done():
			return ullm.Response{}, ctx.Err()
		}
	}
	if step.Err != nil {
		return ullm.Response{}, step.Err
	}
	stop := step.Stop
	if stop == "" {
		stop = ullm.StopComplete
	}
	usage := step.Usage
	if usage.InputTokens == 0 && usage.OutputTokens == 0 && len(usage.Raw) == 0 {
		usage = ullm.Usage{InputTokens: int64(100 * len(r.Input)), OutputTokens: 10}
	}
	return ullm.Response{
		ID:     fmt.Sprintf("fake-%d", n),
		Stop:   stop,
		Output: append([]ullm.Item(nil), step.Output...),
		Usage:  usage,
	}, nil
}

func timerChan(t *time.Timer) <-chan struct{} {
	ch := make(chan struct{})
	go func() {
		<-t.C
		close(ch)
	}()
	return ch
}

func textResponse(n int, text string) ullm.Response {
	return ullm.Response{
		ID:     fmt.Sprintf("fake-%d", n),
		Stop:   ullm.StopComplete,
		Output: []ullm.Item{Text(text)},
		Usage:  ullm.Usage{InputTokens: 1, OutputTokens: 1},
	}
}

func mismatch(s Step, r ullm.Request) string {
	if s.Match != nil {
		if err := s.Match(r); err != nil {
			return err.Error()
		}
	}
	if s.Want != "" {
		last := LastUserText(r.Input)
		if !strings.Contains(last, s.Want) {
			return fmt.Sprintf("want %q in the last user-side text, got %q", s.Want, last)
		}
	}
	return ""
}

// LastUserText is the text of the trailing user-side run of the input:
// user messages and tool result text, joined by newlines. It is what a
// step's Want is matched against.
func LastUserText(in []ullm.Item) string {
	start := len(in)
	for start > 0 && !assistantSide(in[start-1]) && !(start-1 == 0 && isSystem(in[0])) {
		start--
	}
	var parts []string
	for _, it := range in[start:] {
		switch d := it.Data.(type) {
		case ullm.Message:
			parts = append(parts, d.Text)
		case ullm.ToolResult:
			for _, o := range d.Output {
				if o.Kind == ullm.ToolResultText {
					parts = append(parts, o.Value)
				}
			}
		}
	}
	return strings.Join(parts, "\n")
}

func assistantSide(it ullm.Item) bool {
	switch d := it.Data.(type) {
	case ullm.ToolCall, ullm.Reasoning:
		return true
	case ullm.Message:
		return d.Role == ullm.RoleAssistant
	}
	return false
}

func isSystem(it ullm.Item) bool {
	m, ok := it.Data.(ullm.Message)
	return ok && m.Role == ullm.RoleSystem
}

// Text is an assistant message.
func Text(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: s}}
}

// Call is a tool call with verbatim JSON arguments.
func Call(id, name, argsJSON string) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id, Name: name, Arguments: argsJSON}}
}

// Think is a reasoning item shaped like a signed Anthropic thinking
// block, so the Messages render keeps it.
func Think(summary string) ullm.Item {
	raw, _ := json.Marshal(struct {
		Type      string `json:"type"`
		Thinking  string `json:"thinking"`
		Signature string `json:"signature"`
	}{"thinking", summary, "fake"}, json.Deterministic(true))
	r := ullm.Reasoning{Raw: jsontext.Value(raw)}
	if summary != "" {
		r.Summary = []string{summary}
	}
	return ullm.Item{Type: ullm.ItemReasoning, Data: r}
}

// Render is a readable dump of a request, for failure messages.
func Render(r ullm.Request) string {
	var b strings.Builder
	fmt.Fprintf(&b, "model=%s effort=%s tools=%d\n", r.Model.ID, r.Model.ReasoningEffort, len(r.Tools))
	for i, it := range r.Input {
		switch d := it.Data.(type) {
		case ullm.Message:
			fmt.Fprintf(&b, "%3d %s: %s\n", i, d.Role, clip(d.Text))
		case ullm.ToolCall:
			fmt.Fprintf(&b, "%3d call %s %s %s\n", i, d.CallID, d.Name, clip(d.Arguments))
		case ullm.ToolResult:
			var parts []string
			for _, o := range d.Output {
				parts = append(parts, string(o.Kind)+":"+clip(o.Value))
			}
			fmt.Fprintf(&b, "%3d result %s %s\n", i, d.CallID, strings.Join(parts, " | "))
		case ullm.Reasoning:
			fmt.Fprintf(&b, "%3d reasoning %q\n", i, d.Summary)
		}
	}
	return b.String()
}

func clip(s string) string {
	s = strings.ReplaceAll(s, "\n", `\n`)
	if len(s) > 200 {
		return s[:200] + "…"
	}
	return s
}

// clone deep-copies a request through its JSON form, which is what the
// store would hold, so a caller mutating its builder later cannot
// change what was recorded.
func clone(r ullm.Request) ullm.Request {
	out := r
	out.Input = make([]ullm.Item, len(r.Input))
	for i, it := range r.Input {
		b, err := json.Marshal(it)
		if err != nil {
			out.Input[i] = it
			continue
		}
		var c ullm.Item
		if json.Unmarshal(b, &c) != nil {
			out.Input[i] = it
			continue
		}
		out.Input[i] = c
	}
	out.Tools = append([]ullm.Tool(nil), r.Tools...)
	if r.Model.MaxOutputTokens != nil {
		v := *r.Model.MaxOutputTokens
		out.Model.MaxOutputTokens = &v
	}
	return out
}
