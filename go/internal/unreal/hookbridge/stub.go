// Package hookbridge is W5's (go/docs/unreal-engine.md §11.4). This file
// is the B0 stub W2 wrote because B0 never ran: the frozen signatures,
// running every call unhooked, so plugins/engine can wire the bridge now
// and get W5's real one by deleting this file at integration.
package hookbridge

import (
	"context"
	"encoding/json"

	"github.com/andreylukin/bough/internal/agenttools"
)

type Firer interface {
	Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error)
	TakeFireRecords() []map[string]any
}

type Bridge struct {
	get func() (Firer, bool)

	// Session and Notify are W5's (additive to §11.4): the ledger's
	// session id and the live line for a hook's notice, error or
	// rewrite. The stub carries them so plugins/engine compiles against
	// either file.
	Session string
	Notify  func(kind, text string)
}

var _ agenttools.Hooks = (*Bridge)(nil)

func New(get func() (Firer, bool)) *Bridge { return &Bridge{get: get} }

func (b *Bridge) PreTool(ctx context.Context, tool string, c agenttools.Call, detail string) (json.RawMessage, string) {
	return nil, ""
}

func (b *Bridge) PostTool(ctx context.Context, tool string, c agenttools.Call, detail string, r agenttools.Result) agenttools.Result {
	return r
}

func (b *Bridge) SessionStart(ctx context.Context) string { return "" }

func (b *Bridge) PromptSubmit(ctx context.Context, text string) (string, string, []string) {
	return text, "", nil
}

func (b *Bridge) Stop(ctx context.Context, reply string) string { return "" }

func (b *Bridge) Drain() []map[string]any { return nil }
