//go:build !windows

// Package project turns what the harness recorded into what bough's
// readers already understand. The store is the engine's truth; history
// entries are the truth for serve, the web, the TUI, title, cost and
// every past session, so each content row a harness item carries is
// projected onto the kinds and data keys those readers consume
// (go/docs/unreal-engine.md §10.1, the P rows). Turn structure (input,
// done, cancelled, job adoption) is the session actor's; this package
// never writes it.
//
// The Projector is pure: no I/O, no clock, no goroutines. Feed it every
// store item of a session in Sequence order, the Gate's Meta before the
// ModelResponse it describes, and the live Delta and Progress values as
// they happen. To rebuild state after a restart without writing rows
// twice, feed the already-projected items too and drop their Outs: a
// call whose ModelResponse was never seen still projects, because a
// bough.call op carries its tool and arguments in its plan.
package project

import (
	"encoding/json"
	"strconv"
	"strings"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/tool"
)

// Out is one projected row. A recorded Out's Data is the history entry's
// data verbatim (it carries "text" and "hseq"), so the caller appends
// Data as it is and emits the event with the same Kind, Text and Data.
type Out struct {
	Kind   string
	Text   string
	Data   map[string]any // includes "hseq" on every Record entry
	Record bool           // true: history entry + event; false: event only
}

// Meta is one per real or gated Respond, keyed by Response.ID.
type Meta struct {
	ResponseID string
	Model      string
	Provider   string // the llm row's provenance name, as loop.go:819-834 records it
	Muted      bool   // the Gate answered without the provider
	Partial    bool   // a cancelled stream; Output holds its text
	Err        string // provider error the Gate converted
	Overflow   bool   // Err is Model's context overflow (sticky for it)
}

type Config struct {
	Prefix    string // "" or "sub:"
	Worker    string
	Detail    func(tool string, args json.RawMessage) string
	Render    func(callID string, status tool.CallStatus, ops []operation.Operation) (string, toolreg.Handle, bool)
	JS        string // name of the run_js tool ("" = none): its calls project as code/result
	RowOutput int    // bytes of output kept on a call entry (head+tail)
}

// DefaultRowOutput is what a call entry keeps of its output when Config
// leaves RowOutput at 0. The history file is read whole by serve on
// every page load, so a build log must not ride along uncut; 8 KiB is
// what the web's expanded row shows before it says the rest was cut.
const DefaultRowOutput = 8 << 10

// cmdCap bounds the full bash command kept on a call entry for serve's
// test matching: enough for any real `go test` line, never a heredoc.
const cmdCap = 2 << 10

type call struct {
	tool    string
	args    json.RawMessage
	detail  string
	firstAt int64 // RecordedAt of the first status, unix ms; 0 = not yet
	started bool  // the first status was projected
	done    bool  // the terminal row was projected
	late    bool  // an ItemTurn happened while it ran
	job     int   // adopted as this job number (0 = not adopted)
}

type Projector struct {
	c     Config
	meta  map[string]Meta
	calls map[string]*call
	order []string // running call ids, oldest first, for the late mark

	seq     uint64 // the newest delta stream seen
	attempt int
	shown   bool // partial text of (seq, attempt) reached a reader
}

func New(c Config) *Projector {
	if c.RowOutput <= 0 {
		c.RowOutput = DefaultRowOutput
	}
	return &Projector{c: c, meta: map[string]Meta{}, calls: map[string]*call{}}
}

// Meta must precede the ModelResponse it describes.
func (p *Projector) Meta(m Meta) { p.meta[m.ResponseID] = m }

// Call reports what the projector knows about a call: its tool and the
// detail its row shows. The actor names adopted jobs by it.
func (p *Projector) Call(callID string) (tool, detail string, ok bool) {
	c, ok := p.calls[callID]
	if !ok {
		return "", "", false
	}
	return c.tool, c.detail, true
}

// Adopt marks a call the actor numbered as a job when its turn settled
// with the call still running (§9.3). Its terminal row then carries the
// job number and "adopted", so the TUI and web mark it (bg) instead of
// reading it as a step of the turn that is now long closed.
func (p *Projector) Adopt(callID string, job int) {
	if c, ok := p.calls[callID]; ok {
		c.job = job
	}
}

// Item projects one store item: content rows only (§10.2 column P).
func (p *Projector) Item(it sessionstore.Item) []Out {
	switch it.Kind {
	case sessionstore.ItemInput:
		in, _ := it.Data.(inbox.Input)
		return p.input(it, in)
	case sessionstore.ItemTurn:
		// A call still running when the model is asked again was shown
		// to it as the placeholder: its result reaches the model late,
		// and the row says so.
		for _, id := range p.order {
			if c := p.calls[id]; c != nil && c.started && !c.done {
				c.late = true
			}
		}
		return nil
	case sessionstore.ItemModelResponse:
		mr, _ := it.Data.(sessionstore.ModelResponse)
		return p.response(it, mr.Response)
	case sessionstore.ItemToolCallStatus:
		st, _ := it.Data.(sessionstore.ToolCallStatus)
		return p.status(it, st)
	}
	return nil
}

func (p *Projector) input(it sessionstore.Item, in inbox.Input) []Out {
	if in.Kind != inbox.InputControl {
		return nil // external: the actor recorded it before Submit
	}
	msg, err := in.DecodeControlMessage()
	if err != nil || msg.Mode != inbox.Heartbeat || p.c.Prefix != "" {
		return nil
	}
	return []Out{p.rec(it, "system", msg.Reason, map[string]any{"heartbeat": true})}
}

func (p *Projector) response(it sessionstore.Item, r ullm.Response) []Out {
	m, hasMeta := p.meta[r.ID]
	delete(p.meta, r.ID)
	// The streamed text of this request is superseded by what follows
	// (or by nothing, when muted): the next request's deltas start clean.
	p.shown = false
	if m.Muted || m.Err != "" {
		// Muted: the Gate answered without the provider, there is no
		// content. Err: the actor records the error and closes the turn.
		return nil
	}
	var out []Out
	for _, item := range r.Output {
		switch item.Type {
		case ullm.ItemReasoning:
			rs, ok := item.Data.(ullm.Reasoning)
			if !ok || len(rs.Summary) == 0 || p.c.Prefix != "" {
				continue
			}
			text := strings.Join(rs.Summary, "\n\n")
			if strings.TrimSpace(text) == "" {
				continue
			}
			out = append(out, p.rec(it, "thinking", text, nil))
		case ullm.ItemMessage:
			msg, ok := item.Data.(ullm.Message)
			if !ok || msg.Role != ullm.RoleAssistant || strings.TrimSpace(msg.Text) == "" {
				continue
			}
			data := map[string]any{}
			if hasMeta {
				if m.Model != "" {
					data["model"] = m.Model
				}
				if m.Provider != "" {
					data["provider"] = m.Provider
				}
				if m.Partial {
					data["partial"] = true
				}
			}
			out = append(out, p.rec(it, "assistant", msg.Text, data))
		case ullm.ItemToolCall:
			tc, ok := item.Data.(ullm.ToolCall)
			if !ok {
				continue
			}
			p.learn(tc.CallID, tc.Name, json.RawMessage(tc.Arguments))
		}
	}
	if r.Stop == ullm.StopMaxOutputTokens && p.c.Prefix == "" {
		out = append(out, p.rec(it, "system", "reply cut off at max_tokens; raise the llm row's max_tokens if this keeps happening", nil))
	}
	return out
}

// learn registers a call the model made, before its first status.
func (p *Projector) learn(id, name string, args json.RawMessage) *call {
	if c, ok := p.calls[id]; ok {
		return c
	}
	c := &call{tool: name, args: args}
	c.detail = p.detail(name, args)
	p.calls[id] = c
	p.order = append(p.order, id)
	return c
}

func (p *Projector) detail(name string, args json.RawMessage) string {
	if name == p.c.JS && p.c.JS != "" {
		return jsCode(args)
	}
	if p.c.Detail != nil {
		if d := p.c.Detail(name, args); d != "" {
			return d
		}
	}
	return name
}

func (p *Projector) status(it sessionstore.Item, st sessionstore.ToolCallStatus) []Out {
	c, ok := p.calls[st.CallID]
	if !ok {
		// A call whose ModelResponse this projector never saw (a
		// catch-up that starts mid-session): the plan names it.
		name, args := planOf(st.Operations)
		if name == "" {
			name = "tool"
		}
		c = p.learn(st.CallID, name, args)
	}
	if c.done {
		return nil
	}
	at := it.RecordedAt.UnixMilli()
	text, h, done := p.render(st)
	var out []Out
	if !c.started {
		c.started, c.firstAt = true, at
		if st.Status.Error != "" {
			c.done = true
			p.forget(st.CallID)
			msg := strings.TrimPrefix(st.Status.Error, "Error: ")
			if p.js(c) {
				if code := jsCode(c.args); code != "" {
					out = append(out, p.rec(it, "code", code, nil))
				}
				return append(out, p.rec(it, "result", "Error: "+msg, map[string]any{"code": jsCode(c.args), "ms": 0, "error": msg}))
			}
			return append(out, p.rec(it, "call", c.detail, p.callData(st.CallID, c, map[string]any{"ms": 0, "error": msg})))
		}
		if p.js(c) {
			out = append(out, p.rec(it, "code", c.detail, nil))
		} else if !done {
			// A status that is already terminal (a translator that
			// answered without an op, or a catch-up past the start)
			// gets its recorded row only: a start row the end replaces
			// in the same instant is noise.
			out = append(out, p.live("call", c.detail, p.callData(st.CallID, c, map[string]any{"phase": "start"})))
		}
	}
	if !done {
		return out
	}
	c.done = true
	p.forget(st.CallID)
	ms := at - c.firstAt
	if ms <= 0 && h.MS > 0 {
		ms = h.MS
	}
	canceled, failed := false, false
	for _, op := range st.Operations {
		switch op.Status {
		case operation.StatusCanceled:
			canceled = true
		case operation.StatusFailed:
			failed = true
		}
	}
	errText := ""
	switch {
	case canceled:
		errText = strings.TrimPrefix(text, "Cancelled: ")
		if h.Error != "" {
			errText = h.Error
		}
	case failed:
		errText = strings.TrimPrefix(text, "Error: ")
		if errText == "" {
			errText = h.Error
		}
	}
	if p.js(c) {
		data := map[string]any{"code": jsCode(c.args), "ms": ms}
		if v, ok := h.Data["exit"]; ok {
			data["exit"] = v
		}
		if errText != "" {
			data["error"] = errText
			// A failed block reads as an error before its result, as the
			// loop's does; the recorded result carries the error key.
			out = append(out, p.live("error", errText, nil))
		}
		return append(out, p.rec(it, "result", text, data))
	}
	data := map[string]any{"ms": ms}
	for _, k := range []string{"exit", "add", "del", "job"} {
		if v, ok := h.Data[k]; ok {
			data[k] = v
		}
	}
	if cmd, ok := h.Data["cmd"].(string); ok && cmd != "" {
		data["cmd"] = headTail(cmd, cmdCap)
	}
	if errText != "" || canceled {
		data["error"] = errText
	}
	if canceled {
		data["canceled"] = true
	}
	if c.late {
		data["late"] = true
	}
	if c.job > 0 {
		data["job"] = c.job
		data["adopted"] = true
	}
	kept := headTail(text, p.c.RowOutput)
	if kept != "" {
		data["output"] = kept
	}
	if kept != text || h.Truncated {
		data["truncated"] = true
	}
	return append(out, p.rec(it, "call", c.detail, p.callData(st.CallID, c, data)))
}

// render asks toolreg for a bough.call op's text; anything else (the
// harness view_image op, or a translator that answered without an op)
// is read here, because toolreg.Render only knows bough.call.
func (p *Projector) render(st sessionstore.ToolCallStatus) (string, toolreg.Handle, bool) {
	remote := len(st.Operations) > 0
	for _, op := range st.Operations {
		if op.Type != operation.TypeRemoteJob {
			remote = false
		}
	}
	if remote && p.c.Render != nil {
		return p.c.Render(st.CallID, st.Status, st.Operations)
	}
	var h toolreg.Handle
	var texts []string
	for _, op := range st.Operations {
		if !terminal(op.Status) {
			return "", h, false
		}
		switch op.Type {
		case operation.TypeViewImage:
			s, err := operation.DecodeViewImageState(op)
			if err != nil || s.Result == nil {
				continue
			}
			// Never the image bytes: a history entry is read whole by
			// every reader, and the base64 would be the whole of it.
			if s.Result.Error != "" {
				texts = append(texts, "Error: "+s.Result.Error)
				continue
			}
			texts = append(texts, s.Path+" ("+strconv.Itoa(s.Result.OriginalWidth)+"×"+strconv.Itoa(s.Result.OriginalHeight)+" "+s.Result.OriginalMIMEType+")")
		case operation.TypeRemoteJob:
			s, err := operation.DecodeRemoteJobState(op)
			if err != nil {
				continue
			}
			if s.TerminalError != "" {
				texts = append(texts, "Error: "+s.TerminalError)
			} else {
				texts = append(texts, s.TerminalResult)
			}
		}
	}
	return strings.Join(texts, "\n"), h, true
}

func terminal(s operation.Status) bool {
	return s == operation.StatusCompleted || s == operation.StatusFailed || s == operation.StatusCanceled
}

func (p *Projector) forget(id string) {
	for i, v := range p.order {
		if v == id {
			p.order = append(p.order[:i], p.order[i+1:]...)
			return
		}
	}
}

func (p *Projector) js(c *call) bool { return p.c.JS != "" && c.tool == p.c.JS }

// callData is a call row's identity: the tool, the provider call id
// (what pairs the recorded end with its live start), and the worker.
func (p *Projector) callData(id string, c *call, data map[string]any) map[string]any {
	data["tool"] = c.tool
	data["id"] = id
	return data
}

// planOf reads the tool and arguments out of a bough.call op's plan.
func planOf(ops []operation.Operation) (string, json.RawMessage) {
	for _, op := range ops {
		s, err := operation.DecodeRemoteJobState(op)
		if err != nil || s.Plan.Type != toolreg.PlanType {
			continue
		}
		var pl toolreg.Plan
		if json.Unmarshal(s.Plan.Data, &pl) == nil && pl.Tool != "" {
			return pl.Tool, pl.Args
		}
	}
	return "", nil
}

func jsCode(args json.RawMessage) string {
	var a struct {
		Code string `json:"code"`
	}
	_ = json.Unmarshal(args, &a)
	return a.Code
}

// Delta projects one live fragment of a model request. Only the newest
// request streams; when a newer request or a retry starts after partial
// text was shown, a delta-reset tells readers to drop it, because the
// text that follows does not continue it.
func (p *Projector) Delta(d agentllm.Delta) []Out {
	if p.c.Prefix != "" {
		// A subagent's reply lands whole on its card; its stream would
		// interleave with the parent's live text in one pane.
		return nil
	}
	if d.Seq < p.seq {
		return nil
	}
	var out []Out
	if d.Seq != p.seq || d.Attempt != p.attempt {
		if p.shown {
			out = append(out, p.live("delta-reset", "", map[string]any{"seq": p.seq}))
		}
		p.seq, p.attempt, p.shown = d.Seq, d.Attempt, false
	}
	switch d.Kind {
	case agentllm.DeltaText:
		if d.Text != "" {
			p.shown = true
			out = append(out, p.live("assistant-delta", d.Text, nil))
		}
	case agentllm.DeltaThinking:
		if d.Text != "" {
			p.shown = true
			out = append(out, p.live("thinking-delta", d.Text, nil))
		}
	case agentllm.DeltaToolStart:
		if d.Name != "" {
			out = append(out, p.live("activity", "writing "+d.Name+" call", nil))
		}
	case agentllm.DeltaRetry:
		out = append(out, p.live("system", "provider hiccup — retrying…", nil))
	}
	return out
}

// Progress projects live output of a running call: the tail its row
// shows until the recorded end replaces it.
func (p *Projector) Progress(callID, text string) []Out {
	if text == "" || p.c.Prefix != "" {
		return nil
	}
	return []Out{p.live("call-delta", text, map[string]any{"id": callID})}
}

func (p *Projector) rec(it sessionstore.Item, kind, text string, data map[string]any) Out {
	if data == nil {
		data = map[string]any{}
	}
	data["text"] = text
	data["hseq"] = uint64(it.Sequence)
	p.worker(data)
	return Out{Kind: p.c.Prefix + kind, Text: text, Data: data, Record: true}
}

func (p *Projector) live(kind, text string, data map[string]any) Out {
	if data == nil {
		data = map[string]any{}
	}
	p.worker(data)
	return Out{Kind: p.c.Prefix + kind, Text: text, Data: data}
}

// worker stamps a child's rows. Readers key a subagent's card on a
// worker NUMBER (plugins/ui/spawn.go workerOf, the web's sub grouping),
// so a numeric name is written as one.
func (p *Projector) worker(data map[string]any) {
	if p.c.Worker == "" {
		return
	}
	if n, err := strconv.Atoi(p.c.Worker); err == nil {
		data["worker"] = n
		return
	}
	data["worker"] = p.c.Worker
}

// headTail keeps the first and last parts of s within n bytes, cut on
// rune boundaries: the start says what ran and the end how it ended.
func headTail(s string, n int) string {
	if len(s) <= n {
		return s
	}
	const mark = "\n…\n"
	keep := n - len(mark)
	if keep <= 0 {
		return s[:runeCut(s, n)]
	}
	head := runeCut(s, keep/2)
	tail := len(s) - (keep - head)
	for tail < len(s) && !utf8Start(s[tail]) {
		tail++
	}
	return s[:head] + mark + s[tail:]
}

func runeCut(s string, n int) int {
	if n >= len(s) {
		return len(s)
	}
	for n > 0 && !utf8Start(s[n]) {
		n--
	}
	return n
}

func utf8Start(b byte) bool { return b&0xC0 != 0x80 }
