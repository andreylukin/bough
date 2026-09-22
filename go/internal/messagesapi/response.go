package messagesapi

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

// errPauseTurn is a stop only server tools produce; bough sends none, so
// it means the request was not what bough thinks it sent.
var errPauseTurn = errors.New("messagesapi: the model paused its turn (pause_turn), which only server tools cause and bough sends none")

// Decode turns an accumulated message into a harness response. Every
// lossy decision (which blocks survive, what a stop means) is made here,
// once; Render never second-guesses what Decode kept.
func Decode(m anthropic.BetaMessage) (ullm.Response, error) {
	resp := ullm.Response{ID: m.ID, Usage: usage(m)}

	// A server-side fallback leaves a fallback block where the declined
	// model stopped. Its thinking and tool calls before that point must
	// not be echoed back (the claude-api skill's echo rule); its text may.
	lastFallback := -1
	for i, b := range m.Content {
		if b.Type == "fallback" {
			lastFallback = i
		}
	}
	for i, b := range m.Content {
		before := i < lastFallback
		switch b.Type {
		case "thinking":
			if before || b.Signature == "" {
				// An unsigned block cannot be sent back (a cut stream
				// leaves one), and nothing else reads it.
				continue
			}
			raw, err := json.Marshal(rawBlock{Type: "thinking", Thinking: b.Thinking, Signature: b.Signature}, json.Deterministic(true))
			if err != nil {
				return ullm.Response{}, fmt.Errorf("messagesapi: encode thinking: %w", err)
			}
			r := ullm.Reasoning{Raw: jsontext.Value(raw)}
			if b.Thinking != "" {
				r.Summary = []string{b.Thinking}
			}
			resp.Output = append(resp.Output, ullm.Item{Type: ullm.ItemReasoning, Data: r})
		case "redacted_thinking":
			if before || b.Data == "" {
				continue
			}
			raw, err := json.Marshal(rawBlock{Type: "redacted_thinking", Data: b.Data}, json.Deterministic(true))
			if err != nil {
				return ullm.Response{}, fmt.Errorf("messagesapi: encode redacted thinking: %w", err)
			}
			resp.Output = append(resp.Output, ullm.Item{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Raw: jsontext.Value(raw)}})
		case "text":
			if b.Text == "" {
				continue
			}
			resp.Output = append(resp.Output, ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: b.Text}})
		case "tool_use":
			if before {
				continue
			}
			args := string(b.Input)
			if args == "" {
				args = "{}"
			}
			resp.Output = append(resp.Output, ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: b.ID, Name: b.Name, Arguments: args}})
		}
		// fallback, server_tool_use and unknown blocks: dropped.
	}

	switch m.StopReason {
	case anthropic.BetaStopReasonEndTurn, anthropic.BetaStopReasonToolUse, anthropic.BetaStopReasonStopSequence:
		resp.Stop = ullm.StopComplete
	case anthropic.BetaStopReasonMaxTokens, anthropic.BetaStopReasonModelContextWindowExceeded:
		resp.Stop = ullm.StopMaxOutputTokens
		// A call cut off mid-arguments is not a call the model made.
		if n := len(resp.Output); n > 0 && resp.Output[n-1].Type == ullm.ItemToolCall {
			resp.Output = resp.Output[:n-1]
		}
	case anthropic.BetaStopReasonRefusal:
		resp.Stop = ullm.StopRefused
		kept := resp.Output[:0]
		for _, it := range resp.Output {
			if it.Type != ullm.ItemToolCall {
				kept = append(kept, it)
			}
		}
		resp.Output = kept
		resp.Failure = &ullm.Failure{
			Code:    "refusal:" + string(m.StopDetails.Category),
			Message: m.StopDetails.Explanation,
		}
	case anthropic.BetaStopReasonPauseTurn:
		return ullm.Response{}, errPauseTurn
	case "":
		return ullm.Response{}, fmt.Errorf("messagesapi: the response has no stop reason: %w", errCut)
	default:
		return ullm.Response{}, fmt.Errorf("messagesapi: unsupported stop reason %q", m.StopReason)
	}
	if resp.Output == nil {
		resp.Output = []ullm.Item{}
	}
	return resp, nil
}

// usage maps Anthropic's split counts onto the harness's inclusive ones:
// InputTokens covers the cached and the freshly written prefix too, as
// every other provider reports it.
//
// Raw is the message's usage object as the stream left it: Accumulate
// merges each message_delta's usage into the message JSON but not into
// the Usage field's own raw bytes, which still hold message_start's.
func usage(m anthropic.BetaMessage) ullm.Usage {
	u := m.Usage
	out := ullm.Usage{
		InputTokens:           u.InputTokens + u.CacheReadInputTokens + u.CacheCreationInputTokens,
		CachedInputTokens:     u.CacheReadInputTokens,
		CacheWriteInputTokens: u.CacheCreationInputTokens,
		OutputTokens:          u.OutputTokens,
		ReasoningTokens:       u.OutputTokensDetails.ThinkingTokens,
	}
	var env struct {
		Usage jsontext.Value `json:"usage"`
	}
	if raw := m.RawJSON(); raw != "" && json.Unmarshal([]byte(raw), &env) == nil && env.Usage.Kind() == '{' {
		out.Raw = env.Usage
	} else if raw := u.RawJSON(); raw != "" && jsontext.Value(raw).IsValid() {
		out.Raw = jsontext.Value(raw)
	}
	return out
}
