// Package boughcall runs bough.call remote jobs: the in-process
// operation.RemoteJobHandler behind every native tool call. It is the
// one place a call touches the world — hooks, the tool's Go function,
// redaction, spilling — so that nothing a translator or the store sees
// was ever a pre-redaction string (docs/unreal-engine.md §8.2).
//
// The operation manager calls AddRemoteJob and CancelRemoteJob on its
// own goroutine and reads RemoteJobUpdates on another; a send from
// either call would deadlock it. So every update goes through an
// unbounded FIFO drained by one pump goroutine, which also keeps an
// op's updates in the order they happened.
package boughcall

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/unreallabsai/unreal-agent/harness/operation"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

// ErrInterrupted is the failure of a call that was running when the
// previous bough process ended: it is not re-run, because a half-run
// command run again is worse than one reported as lost.
var ErrInterrupted = errors.New("interrupted: bough restarted before this call finished; it was not re-run")

// cancelGrace is how long a cancelled call's tool gets to return before
// the op is reported canceled anyway: a tool that ignores its context
// must not leave the op in canceling forever.
const cancelGrace = 2 * time.Second

type Options struct {
	Tools       func(name string) (agenttools.Tool, bool) // the live agent-tools registry
	Hooks       agenttools.Hooks                          // may be nil
	Redact      func(string) string                       // may be nil; applied before any state is written
	CallTimeout time.Duration
	SpillDir    func() string // $BOUGH_SCRATCH/calls
	Session     string
	Worker      string
	Progress    func(callID, text string) // → call-delta (coalesced by the actor)
}

// Handler implements operation.RemoteJobHandler for plan bough.call v1.
type Handler struct {
	o     Options
	grace time.Duration // cancelGrace; tests shorten it

	ctx    context.Context // ends at Close: every running call is cancelled with it
	cancel context.CancelFunc

	mu      sync.Mutex
	queue   []operation.Operation
	running map[operation.ID]*run
	closed  bool
	kick    chan struct{} // buffered 1: the pump has work
	updates chan operation.Operation
	pumped  chan struct{} // closed when the pump has exited
}

// run is one call in flight. finished is set exactly once, by whoever
// reports its terminal state first: the call returning, or a cancel.
type run struct {
	op       operation.Operation // the awaiting snapshot every later update derives from
	cancel   context.CancelFunc
	returned chan struct{}
	finished bool
}

var _ operation.RemoteJobHandler = (*Handler)(nil)

func New(o Options) *Handler {
	ctx, cancel := context.WithCancel(context.Background())
	h := &Handler{
		o:       o,
		grace:   cancelGrace,
		ctx:     ctx,
		cancel:  cancel,
		running: map[operation.ID]*run{},
		kick:    make(chan struct{}, 1),
		updates: make(chan operation.Operation),
		pumped:  make(chan struct{}),
	}
	go h.pump()
	return h
}

func (*Handler) RemoteJobPlanType() operation.RemoteJobPlanType { return toolreg.PlanType }
func (*Handler) RemoteJobPlanVersion() operation.RemoteJobPlanVersion {
	return toolreg.PlanVersion
}
func (h *Handler) RemoteJobUpdates() <-chan operation.Operation { return h.updates }

// Close cancels every running call and closes the update channel; the
// manager then fails whatever it still tracks for this handler.
func (h *Handler) Close() error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil
	}
	h.closed = true
	h.mu.Unlock()
	h.cancel()
	<-h.pumped
	return nil
}

func (h *Handler) AddRemoteJob(op operation.Operation) error {
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil {
		return err
	}
	switch op.Status {
	case operation.StatusAwaiting:
		// Re-added from a store: the process that ran it is gone.
		step, err := operation.FailRemoteJob(op, ErrInterrupted)
		if err != nil {
			return err
		}
		h.push(*step.Operation)
		return nil
	case operation.StatusCanceling:
		step, err := operation.CancelRemoteJob(op)
		if err != nil {
			return err
		}
		h.push(*step.Operation)
		return nil
	case operation.StatusReady:
	default:
		return nil // terminal: nothing to run
	}
	var plan toolreg.Plan
	if err := json.Unmarshal(state.Plan.Data, &plan); err != nil {
		return fmt.Errorf("boughcall: decode plan of %s: %w", op.ID, err)
	}
	step, err := operation.UpdateRemoteJob(op, state, operation.StatusAwaiting)
	if err != nil {
		return err
	}
	awaiting := *step.Operation

	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return fmt.Errorf("boughcall: handler closed")
	}
	if _, dup := h.running[op.ID]; dup {
		h.mu.Unlock()
		return nil
	}
	ctx, cancel := context.WithCancel(h.ctx)
	r := &run{op: awaiting, cancel: cancel, returned: make(chan struct{})}
	h.running[op.ID] = r
	h.queue = append(h.queue, awaiting)
	h.mu.Unlock()
	h.wake()

	go h.execute(ctx, r, plan)
	return nil
}

// CancelRemoteJob stops a running call: canceling now, canceled once
// its tool returns (or cancelGrace passes). An unknown id is a call
// that already finished.
func (h *Handler) CancelRemoteJob(id operation.ID, reason string) error {
	h.mu.Lock()
	r, ok := h.running[id]
	if !ok || r.finished {
		h.mu.Unlock()
		return nil
	}
	r.finished = true
	if step, err := h.update(r.op, operation.StatusCanceling, nil); err == nil {
		h.queue = append(h.queue, step)
	}
	h.mu.Unlock()
	h.wake()
	r.cancel()
	go func() {
		select {
		case <-r.returned:
		case <-time.After(h.grace):
		}
		h.mu.Lock()
		delete(h.running, id)
		h.mu.Unlock()
		hd := toolreg.Handle{Error: h.redact(reason)}
		if step, err := h.update(r.op, operation.StatusCanceled, &hd); err == nil {
			h.push(step)
		}
	}()
	return nil
}

// update derives r.op's next snapshot. Handle is written only when h
// is non-nil; a canceled op keeps no result text.
func (h *Handler) update(op operation.Operation, status operation.Status, hd *toolreg.Handle) (operation.Operation, error) {
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil {
		return operation.Operation{}, err
	}
	if status == operation.StatusCanceled {
		state.TerminalResult, state.Subscription = "", nil
	}
	if hd != nil {
		state.Handle = encodeHandle(*hd)
	}
	step, err := operation.UpdateRemoteJob(op, state, status)
	if err != nil {
		return operation.Operation{}, err
	}
	return *step.Operation, nil
}

// execute runs one call to its terminal update (§8.2 steps 1–7).
func (h *Handler) execute(ctx context.Context, r *run, plan toolreg.Plan) {
	defer close(r.returned)
	defer r.cancel()
	started := time.Now()
	text, errText, hd := h.call(ctx, plan)
	hd.MS = time.Since(started).Milliseconds()

	h.mu.Lock()
	if r.finished {
		h.mu.Unlock()
		return // cancelled: the cancel path reports it
	}
	r.finished = true
	delete(h.running, r.op.ID)
	h.mu.Unlock()

	state, err := operation.DecodeRemoteJobState(r.op)
	if err != nil {
		return
	}
	status := operation.StatusCompleted
	limit := r.op.MaxOutputLength
	if errText != "" {
		status = operation.StatusFailed
		full := errText
		if text != "" {
			full += "\n" + text
		}
		state.TerminalError, hd = h.bound(plan.Call, full, limit, hd)
		state.TerminalResult = ""
	} else {
		state.TerminalResult, hd = h.bound(plan.Call, text, limit, hd)
	}
	state.Handle = encodeHandle(hd)
	step, err := operation.UpdateRemoteJob(r.op, state, status)
	if err != nil {
		// A state that will not encode is still a failed call, not a
		// silent one.
		if f, ferr := operation.FailRemoteJob(r.op, fmt.Errorf("boughcall: %v", err)); ferr == nil {
			h.push(*f.Operation)
		}
		return
	}
	h.push(*step.Operation)
}

// call is steps 1–5: look up, pre-hook, run, post-hook, redact. It
// returns what the model reads (text), the failure (errText, "" on
// success) and the row's handle.
func (h *Handler) call(ctx context.Context, plan toolreg.Plan) (text, errText string, hd toolreg.Handle) {
	var t agenttools.Tool
	ok := false
	if h.o.Tools != nil {
		t, ok = h.o.Tools(plan.Tool)
	}
	if !ok {
		msg := fmt.Sprintf("tool %q was removed while the call was queued", plan.Tool)
		return "", msg, toolreg.Handle{Error: msg}
	}
	args := plan.Args
	c := agenttools.Call{ID: plan.Call, Args: args, Session: h.o.Session, Worker: h.o.Worker}
	if h.o.Progress != nil {
		c.Progress = func(s string) { h.o.Progress(plan.Call, h.redact(s)) }
	}
	detail := ""
	if t.Detail != nil {
		detail = t.Detail(args)
	}
	if h.o.Hooks != nil {
		newArgs, deny := h.o.Hooks.PreTool(ctx, t.Name, c, detail)
		if deny != "" {
			msg := h.redact("blocked by hook: " + deny)
			return "", msg, toolreg.Handle{Detail: h.redact(detail), Error: msg}
		}
		if newArgs != nil {
			c.Args = newArgs
			if t.Detail != nil {
				detail = t.Detail(newArgs)
			}
		}
	}
	callCtx := ctx
	if h.o.CallTimeout > 0 && !t.Blocking && !ownsTimeout(t, c.Args) {
		var cancel context.CancelFunc
		callCtx, cancel = context.WithTimeout(ctx, h.o.CallTimeout)
		defer cancel()
	}
	res, err := safeCall(callCtx, t, c)
	if err != nil && res.Error == "" {
		res.Error = err.Error()
	}
	if callCtx.Err() == context.DeadlineExceeded && ctx.Err() == nil {
		// Say it was the call timeout, not the tool: "context deadline
		// exceeded" alone reads like the tool's own bug.
		msg := fmt.Sprintf("%s: timed out after %s", t.Name, h.o.CallTimeout)
		if res.Error != "" && !strings.Contains(res.Error, context.DeadlineExceeded.Error()) {
			msg += ": " + res.Error
		}
		res.Error = msg
	}
	if h.o.Hooks != nil {
		res = h.o.Hooks.PostTool(ctx, t.Name, c, detail, res)
	}
	res.Text, res.Error = h.redact(res.Text), h.redact(res.Error)
	hd = toolreg.Handle{Detail: h.redact(detail), Data: redactData(res.Data, h.redact), Error: res.Error}
	return res.Text, res.Error, hd
}

// safeCall turns a panicking tool into a failed call: a tool bug must
// not take the session's operation manager down with it.
func safeCall(ctx context.Context, t agenttools.Tool, c agenttools.Call) (res agenttools.Result, err error) {
	defer func() {
		if p := recover(); p != nil {
			res, err = agenttools.Result{}, fmt.Errorf("%s: tool panic: %v", t.Name, p)
		}
	}()
	return t.Call(ctx, c)
}

// ownsTimeout reports whether a call carries its own deadline (bash's
// timeout argument): the call timeout would otherwise cut a build the
// model explicitly gave half an hour.
func ownsTimeout(t agenttools.Tool, args json.RawMessage) bool {
	props, _ := t.Schema["properties"].(map[string]any)
	if _, ok := props["timeout"]; !ok {
		return false
	}
	var a map[string]json.RawMessage
	if json.Unmarshal(args, &a) != nil {
		return false
	}
	v, ok := a["timeout"]
	return ok && string(v) != "null"
}

func (h *Handler) redact(s string) string {
	if h.o.Redact == nil || s == "" {
		return s
	}
	return h.o.Redact(s)
}

// redactData redacts the string values a tool put on its row (cmd,
// path): they are copied onto the history entry and into the op state.
func redactData(d map[string]any, redact func(string) string) map[string]any {
	if len(d) == 0 {
		return nil
	}
	out := make(map[string]any, len(d))
	for k, v := range d {
		if s, ok := v.(string); ok {
			v = redact(s)
		}
		out[k] = v
	}
	return out
}

// bound keeps text within limit runes. Past it the whole text goes to
// the spill file and the model reads its head and tail with the path
// between them, so nothing is lost, only moved to disk.
func (h *Handler) bound(callID, text string, limit int, hd toolreg.Handle) (string, toolreg.Handle) {
	if limit <= 0 || utf8.RuneCountInString(text) <= limit {
		return text, hd
	}
	hd.Truncated = true
	path := h.spill(callID, text)
	hd.Spill = path
	marker := func(skipped int) string {
		m := fmt.Sprintf("…%d bytes truncated", skipped)
		if path != "" {
			m += "; complete output in " + path
		}
		return m + "…"
	}
	room := limit - utf8.RuneCountInString(marker(len(text)))
	if room < 2 {
		out, _ := operation.BoundOutput(text, limit)
		return out, hd
	}
	head := runePrefix(text, room/2)
	tail := runeSuffix(text, room-room/2)
	return head + marker(len(text)-len(head)-len(tail)) + tail, hd
}

func runePrefix(s string, n int) string {
	i := 0
	for ; n > 0 && i < len(s); n-- {
		_, size := utf8.DecodeRuneInString(s[i:])
		i += size
	}
	return s[:i]
}

func runeSuffix(s string, n int) string {
	i := len(s)
	for ; n > 0 && i > 0; n-- {
		_, size := utf8.DecodeLastRuneInString(s[:i])
		i -= size
	}
	return s[i:]
}

// spill writes the complete text to SpillDir/<callID>.out and returns
// the path, or "" when there is nowhere to write it.
func (h *Handler) spill(callID, text string) string {
	if h.o.SpillDir == nil {
		return ""
	}
	dir := h.o.SpillDir()
	if dir == "" {
		return ""
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return ""
	}
	path := filepath.Join(dir, safeName(callID)+".out")
	if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
		return ""
	}
	return path
}

// safeName keeps a provider call id usable as a file name.
func safeName(id string) string {
	var b strings.Builder
	for _, r := range id {
		if r < 128 && (r == '-' || r == '_' || r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z') {
			b.WriteRune(r)
		} else {
			b.WriteByte('_')
		}
	}
	if b.Len() == 0 {
		return "call"
	}
	return b.String()
}

func encodeHandle(hd toolreg.Handle) jsontext.Value {
	b, err := json.Marshal(hd)
	if err != nil {
		// Data a tool put on the row that will not encode is dropped;
		// the rest of the handle still reaches the row.
		hd.Data = nil
		b, _ = json.Marshal(hd)
	}
	return jsontext.Value(b)
}

func (h *Handler) push(op operation.Operation) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return
	}
	h.queue = append(h.queue, op)
	h.mu.Unlock()
	h.wake()
}

func (h *Handler) wake() {
	select {
	case h.kick <- struct{}{}:
	default:
	}
}

// pump is the only sender on updates. It exits, closing the channel,
// when the handler closes; updates still queued then are dropped, as
// the manager fails every op of a closed handler anyway.
func (h *Handler) pump() {
	defer close(h.pumped)
	defer close(h.updates)
	for {
		h.mu.Lock()
		var next operation.Operation
		has := len(h.queue) > 0
		if has {
			next = h.queue[0]
		}
		h.mu.Unlock()
		if !has {
			select {
			case <-h.kick:
				continue
			case <-h.ctx.Done():
				return
			}
		}
		select {
		case h.updates <- next:
			h.mu.Lock()
			h.queue = h.queue[1:]
			h.mu.Unlock()
		case <-h.ctx.Done():
			return
		}
	}
}
