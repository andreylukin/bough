//go:build !windows

package wrap

// Apart from wrap.go because it reads the Responses API's error type,
// and that package pulls in the harness primitives, which do not build
// for Windows at the pin; the loop's rows build there without it.

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"sync"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/llm/responsesapi"

	"github.com/andreylukin/bough/internal/agentllm"
)

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
	// A reasoning setting the model refuses (reasoning.effort=low on a
	// model that takes only high) names reasoning too, but no replayed
	// item caused it: stripping cannot fix it, and the sticky strip
	// would cost the session its reasoning for nothing.
	if strings.HasPrefix(api.Param, "reasoning.") || api.Code == "unsupported_value" {
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
