// Package hookbridge fires the hooks row's events at the engine's
// lifecycle points and around every native tool call, with the payload
// and result keys the loop uses, so a hook written for the loop keeps
// working on the engine (go/docs/unreal-engine.md §11.4).
//
// It implements agenttools.Hooks (boughcall calls it at the operation
// boundary) and the engine session's Lifecycle. The hooks service is
// looked up on every fire, never held: the hooks row can remount, or
// mount after the engine, and a session with no hooks row pays nothing.
package hookbridge

import (
	"context"
	"encoding/json"
	"strings"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/hookmeta"
)

// Firer is the slice of the hooks service (plugins/hooks.Service) the
// bridge needs, declared here so the engine never imports the row.
type Firer interface {
	Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error)
	TakeFireRecords() []map[string]any
}

// sessionNamer is the hooks ledger's older seam: every fire is recorded
// under the session named before it. It is process-wide, so it is the
// fallback when the service cannot take the session per call.
type sessionNamer interface{ SetSession(id string) }

// sessionFirer fires and drains under a named session: the parent and
// its subagents share one hooks service and fire concurrently.
type sessionFirer interface {
	FireAs(ctx context.Context, session, event string, payload map[string]any) (map[string]any, error)
	TakeFireRecordsFor(session string) []map[string]any
}

// applyingFirer is sessionFirer for a fire site that applies only some
// payload keys, so the ledger records a rewrite only of those.
type applyingFirer interface {
	FireApplying(ctx context.Context, session, event string, payload map[string]any, applies ...string) (map[string]any, error)
}

// Bridge adapts a Firer to the engine's hook points.
type Bridge struct {
	get func() (Firer, bool)

	// Session is the bough session id the fires are recorded under.
	// Set it before the first fire.
	Session string
	// Notify shows a hook's side channel live: kind "system" for a
	// hook's notice to the human, "error" for a hook that failed, and
	// "context" for a rewrite of what the user typed. May be nil: the
	// fire records still carry all three.
	Notify func(kind, text string)
}

var _ agenttools.Hooks = (*Bridge)(nil)

// New returns a Bridge over the hooks service get finds; get reporting
// false (no hooks row) makes every point a no-op.
func New(get func() (Firer, bool)) *Bridge { return &Bridge{get: get} }

func (b *Bridge) notify(kind, text string) {
	if b.Notify != nil {
		b.Notify(kind, text)
	}
}

// fire runs one event the way the loop's runner.fire does: a Fire
// error is shown and never fatal, and the partial result still applies,
// because one throwing hook file must not void what the others said. A
// notice is shown and then dropped from the result, so it can never be
// read as a decision.
func (b *Bridge) fire(ctx context.Context, event string, payload map[string]any, applies ...string) map[string]any {
	if b.get == nil {
		return nil
	}
	h, ok := b.get()
	if !ok || h == nil {
		return nil
	}
	var res map[string]any
	var err error
	if af, ok := h.(applyingFirer); ok && b.Session != "" && applies != nil {
		res, err = af.FireApplying(ctx, b.Session, event, payload, applies...)
	} else if sf, ok := h.(sessionFirer); ok && b.Session != "" {
		res, err = sf.FireAs(ctx, b.Session, event, payload)
	} else {
		if n, ok := h.(sessionNamer); ok && b.Session != "" {
			n.SetSession(b.Session)
		}
		res, err = h.Fire(ctx, event, payload)
	}
	if err != nil {
		b.notify("error", "hook "+event+": "+err.Error())
	}
	if n, ok := res["notice"].(string); ok {
		if n = strings.TrimSpace(n); n != "" {
			b.notify("system", "hook "+event+": "+n)
		}
		delete(res, "notice")
	}
	if len(res) == 0 {
		return nil
	}
	return res
}

// SessionStart fires session-start at the session's first coordinator
// build; its context goes into the frozen prompt (Parts.SessionStart).
func (b *Bridge) SessionStart(ctx context.Context) string {
	c, _ := b.fire(ctx, "session-start", map[string]any{})["context"].(string)
	return c
}

// PromptSubmit fires user-prompt-submit for a line the user typed. A
// "block" reason refuses the turn; an "input" string replaces the line,
// and contexts carries the note saying so, because a hook rewriting
// what you typed must not do it behind your back.
func (b *Bridge) PromptSubmit(ctx context.Context, text string) (out, block string, contexts []string) {
	res := b.fire(ctx, "user-prompt-submit", map[string]any{"input": text})
	if r, ok := res["block"].(string); ok {
		return text, r, nil
	}
	if in, ok := res["input"].(string); ok && in != text {
		note := "hook user-prompt-submit rewrote your message\n" + in
		b.notify("context", note)
		return in, "", []string{note}
	}
	return text, "", nil
}

// Stop fires stop with the reply that closed the turn. A "block" reason
// is what the model is asked to do next instead of stopping.
func (b *Bridge) Stop(ctx context.Context, reply string) string {
	r, _ := b.fire(ctx, "stop", map[string]any{"reply": reply})["block"].(string)
	return r
}

// Drain is every fire recorded since the last drain, as "hook" history
// entries: the ledger lives in this process, and history is the only
// thing `bough serve` can read it from.
func (b *Bridge) Drain() []map[string]any {
	if b.get == nil {
		return nil
	}
	h, ok := b.get()
	if !ok || h == nil {
		return nil
	}
	if sf, ok := h.(sessionFirer); ok && b.Session != "" {
		return sf.TakeFireRecordsFor(b.Session)
	}
	return h.TakeFireRecords()
}

// PreTool fires pre-code-exec before a native call. "code" carries the
// call's whole text (see codeOf), so a hook matching command text
// matches native calls too. A deny or block that refuses
// (hookmeta.Refusal: a string or true) refuses the call, as on the
// loop; an "args" object replaces its arguments. "code" is only read
// here, so the ledger is told a rewrite of it is none.
func (b *Bridge) PreTool(ctx context.Context, tool string, c agenttools.Call, detail string) (json.RawMessage, string) {
	res := b.fire(ctx, "pre-code-exec", map[string]any{
		"code": codeOf(tool, c.Args, detail), "tool": tool, "args": argsObject(c.Args), "call": c.ID,
	}, "args")
	if d, reason := hookmeta.Refusal(res); d != "" {
		return nil, reason
	}
	if a, ok := res["args"].(map[string]any); ok {
		if raw, err := json.Marshal(a); err == nil {
			return raw, ""
		}
	}
	return nil, ""
}

// PostTool fires post-result after a native call. A "result" string
// replaces what the model reads.
func (b *Bridge) PostTool(ctx context.Context, tool string, c agenttools.Call, detail string, r agenttools.Result) agenttools.Result {
	res := b.fire(ctx, "post-result", map[string]any{
		"code": codeOf(tool, c.Args, detail), "tool": tool, "call": c.ID, "result": r.Text, "error": r.Error,
	})
	if s, ok := res["result"].(string); ok {
		r.Text = s
	}
	return r
}

// codeOf is what a hook reads as "code": on the loop it is the whole JS
// block, so a deny matching "git push" sees every line of it. The row's
// detail is only the first line, cut at 80 bytes, and a deny hook
// matched against it let "true\ngit push" or a tools.bash on a block's
// second line through. So bash gives its whole command and run_js its
// whole program; write and patch give the path, as before, then the
// text they put in the file. Any other tool keeps its detail.
func codeOf(tool string, raw json.RawMessage, detail string) string {
	var a struct {
		Command string `json:"command"`
		Code    string `json:"code"`
		Path    string `json:"path"`
		Content string `json:"content"`
		New     string `json:"new"`
	}
	if json.Unmarshal(raw, &a) != nil {
		return detail
	}
	switch tool {
	case "bash":
		if a.Command != "" {
			return a.Command
		}
	case "run_js":
		if a.Code != "" {
			return a.Code
		}
	case "write":
		return a.Path + "\n" + a.Content
	case "patch":
		return a.Path + "\n" + a.New
	}
	return detail
}

// argsObject is the call's arguments as a JS-visible object; a hook
// cannot read raw JSON bytes. Unparseable arguments stay a string, so a
// hook still sees what the model sent.
func argsObject(raw json.RawMessage) any {
	if len(raw) == 0 {
		return map[string]any{}
	}
	var v map[string]any
	if err := json.Unmarshal(raw, &v); err != nil {
		return string(raw)
	}
	return v
}
