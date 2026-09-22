// Package wrap holds the adapter decorators every llm row stacks around
// its harness adapter: provenance envelopes on reasoning, late tool
// results as text for upstreams that reject a second result, a
// self-heal for reasoning a provider refuses to read back, and a usage
// observer. Each decorator keeps the agentllm.Adapter surface, so the
// stack is built by plain composition in the rows.
package wrap

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"net/http"
	"strings"
	"sync"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/llm/responsesapi"

	"github.com/andreylukin/bough/internal/agentllm"
)

// base forwards the identity half of the interface, so a decorator
// only writes Respond.
type base struct{ inner agentllm.Adapter }

func (b base) Provider() string { return b.inner.Provider() }
func (b base) Model() string    { return b.inner.Model() }
func (b base) Close() error     { return b.inner.Close() }

// envelope is what a Reasoning.Raw looks like at rest. The provider is
// stamped when the response is decoded, not sniffed from the bytes
// later, because OpenAI encrypted_content and OpenRouter reasoning are
// indistinguishable by shape and replaying one to the other is a 400.
type envelope struct {
	Bough    int            `json:"bough"`
	Provider string         `json:"provider"`
	Item     jsontext.Value `json:"item"`
}

// Unwrap reads an enveloped Raw: the provider that produced it and the
// verbatim item. ok is false for anything that is not an envelope.
func Unwrap(raw jsontext.Value) (provider string, item jsontext.Value, ok bool) {
	if len(raw) == 0 {
		return "", nil, false
	}
	var e envelope
	if err := json.Unmarshal(raw, &e); err != nil || e.Bough != 1 || e.Provider == "" || len(e.Item) == 0 {
		return "", nil, false
	}
	return e.Provider, e.Item, true
}

// Seal is the inverse of Unwrap.
func Seal(provider string, item jsontext.Value) jsontext.Value {
	b, err := json.Marshal(envelope{Bough: 1, Provider: provider, Item: item}, json.Deterministic(true))
	if err != nil {
		return nil
	}
	return b
}

// Envelope stamps provenance on what a provider returns and strips what
// another provider left behind. A /model switch mid-session keeps the
// store as it was; this is what stops the new provider from being sent
// bytes only the old one can read.
func Envelope(a agentllm.Adapter) agentllm.Adapter { return &enveloped{base{a}} }

type enveloped struct{ base }

func (e *enveloped) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	prov := e.inner.Provider()
	r.Input = openInput(prov, r.Input)
	resp, err := e.inner.Respond(ctx, r, o)
	if err != nil {
		return resp, err
	}
	resp.Output = sealOutput(prov, resp.Output)
	return resp, nil
}

func openInput(prov string, in []ullm.Item) []ullm.Item {
	out := make([]ullm.Item, 0, len(in))
	for _, it := range in {
		if id, ok := strings.CutPrefix(it.ProviderID, prov+"|"); ok {
			it.ProviderID = id
		} else {
			it.ProviderID = ""
		}
		if r, ok := it.Data.(ullm.Reasoning); ok {
			p, item, ok := Unwrap(r.Raw)
			if !ok || p != prov {
				continue
			}
			r.Raw = item
			it.Data = r
		}
		out = append(out, it)
	}
	return out
}

func sealOutput(prov string, in []ullm.Item) []ullm.Item {
	out := make([]ullm.Item, len(in))
	for i, it := range in {
		if it.ProviderID != "" {
			it.ProviderID = prov + "|" + it.ProviderID
		}
		if r, ok := it.Data.(ullm.Reasoning); ok && len(r.Raw) != 0 {
			r.Raw = Seal(prov, r.Raw)
			it.Data = r
		}
		out[i] = it
	}
	return out
}

// Observe calls fn with every successful response, after the inner
// adapter returns it: the rows feed their usage tally from here.
func Observe(a agentllm.Adapter, fn func(ullm.Response)) agentllm.Adapter {
	return &observed{base{a}, fn}
}

type observed struct {
	base
	fn func(ullm.Response)
}

func (o *observed) Respond(ctx context.Context, r ullm.Request, opts ullm.RequestOptions) (ullm.Response, error) {
	resp, err := o.inner.Respond(ctx, r, opts)
	if err == nil && o.fn != nil {
		o.fn(resp)
	}
	return resp, err
}

// StripReasoningOn400 answers a 400 that blames replayed reasoning by
// dropping every Reasoning item and trying once more. The drop is
// sticky for this adapter: a provider that refused one signature will
// refuse the rest of the session's, and asking twice per turn only
// doubles the latency of every request.
func StripReasoningOn400(a agentllm.Adapter) agentllm.Adapter { return &stripping{base: base{a}} }

type stripping struct {
	base
	mu    sync.Mutex
	strip bool
}

func (s *stripping) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	s.mu.Lock()
	strip := s.strip
	s.mu.Unlock()
	if strip {
		r.Input = withoutReasoning(r.Input)
	}
	resp, err := s.inner.Respond(ctx, r, o)
	if err == nil || strip || !reasoning400(err) {
		return resp, err
	}
	s.mu.Lock()
	s.strip = true
	s.mu.Unlock()
	r.Input = withoutReasoning(r.Input)
	return s.inner.Respond(ctx, r, o)
}

func withoutReasoning(in []ullm.Item) []ullm.Item {
	out := make([]ullm.Item, 0, len(in))
	for _, it := range in {
		if it.Type != ullm.ItemReasoning {
			out = append(out, it)
		}
	}
	return out
}

// reasoning400 reports whether err is a 400 that names replayed
// reasoning. The providers word it differently and none has a code
// for it, so this reads the message.
func reasoning400(err error) bool {
	var api *responsesapi.APIError
	if !errors.As(err, &api) || api.StatusCode != http.StatusBadRequest {
		return false
	}
	m := strings.ToLower(api.Message + " " + api.Param + " " + api.Code)
	for _, w := range []string{"reasoning", "thinking", "signature", "encrypted"} {
		if strings.Contains(m, w) {
			return true
		}
	}
	return false
}

// LateResultsAsText turns every tool result an upstream would see as a
// duplicate or out of place into a user message that carries the same
// text. OpenRouter serving an Anthropic model translates Responses input
// into Messages, and a second function_call_output for one call id is a
// 400 there; the text form reads the same to the model and is append-
// only, which keeps the prompt cache.
func LateResultsAsText(a agentllm.Adapter) agentllm.Adapter { return &lateText{base{a}} }

type lateText struct{ base }

// lateImage stands in for an image in a late result: ullm.Message
// carries text only, and the model can fetch the image again.
const lateImage = "[image omitted: call view_image again]"

func (l *lateText) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	late := Late(r.Input)
	names := CallNames(r.Input)
	in := make([]ullm.Item, len(r.Input))
	for i, it := range r.Input {
		in[i] = it
		if !late[i] {
			continue
		}
		res := it.Data.(ullm.ToolResult)
		var parts []string
		for _, out := range res.Output {
			if out.Kind == ullm.ToolResultImage {
				parts = append(parts, lateImage)
				continue
			}
			parts = append(parts, out.Value)
		}
		in[i] = ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{
			Role: ullm.RoleUser,
			Text: LateText(res.CallID, names[res.CallID], strings.Join(parts, "\n")),
		}}
	}
	r.Input = in
	return l.inner.Respond(ctx, r, o)
}
