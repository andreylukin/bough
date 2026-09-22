package messagesapi

import (
	stdjson "encoding/json"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"strings"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/packages/param"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/unreal/wrap"
)

// defaultMaxTokens is max_tokens when neither the request nor the row
// sets one. The adapter always streams, so a large cap costs nothing
// until it is used, and a small one cuts a long file write in half.
const defaultMaxTokens = 64000

// missingResult is what a tool_use with no result renders to. The
// harness gives every call a result before the next request, so this
// only fires on a history the harness did not write (a seed, a crash);
// without it that history is a 400 on every later turn.
const missingResult = "No result was recorded for this call."

// Render is the Messages request for one harness request. It is a pure
// function of r, s and ttl: the same items give the same bytes, which is
// what keeps the prompt cache and Opus 5.5's preserved-thinking check
// intact across requests.
func Render(r ullm.Request, s ModelSpec, ttl string) (anthropic.BetaMessageNewParams, error) {
	var p anthropic.BetaMessageNewParams
	cc := cacheControl(ttl)
	in := r.Input
	if len(in) > 0 {
		if m, ok := in[0].Data.(ullm.Message); ok && m.Role == ullm.RoleSystem {
			if m.Text != "" {
				p.System = []anthropic.BetaTextBlockParam{{Text: m.Text, CacheControl: cc}}
			}
			in = in[1:]
		}
	}
	items, err := renderable(in)
	if err != nil {
		return p, err
	}
	msgs, err := messages(items)
	if err != nil {
		return p, err
	}
	if len(msgs) == 0 {
		return p, errors.New("messagesapi: nothing to send: the request has no messages")
	}
	if msgs[0].Role != anthropic.BetaMessageParamRoleUser {
		return p, errors.New("messagesapi: the first message is from the assistant; the Messages API needs a user message first")
	}
	if msgs[len(msgs)-1].Role != anthropic.BetaMessageParamRoleUser {
		// Prefill is a 400 on every current model; the harness only
		// asks for a reply when something new is on the user side.
		return p, errors.New("messagesapi: the last message is from the assistant (prefill is not accepted)")
	}
	placeMarkers(msgs, cc)
	p.Messages = msgs

	for _, t := range r.Tools {
		if t.Type != ullm.ToolFunction {
			return p, fmt.Errorf("messagesapi: hosted tool %q is not supported on the Messages API", t.Name)
		}
		tool, err := toolParam(t)
		if err != nil {
			return p, err
		}
		p.Tools = append(p.Tools, anthropic.BetaToolUnionParam{OfTool: &tool})
	}

	p.Model = anthropic.Model(r.Model.ID)
	maxTokens := int64(defaultMaxTokens)
	if r.Model.MaxOutputTokens != nil && *r.Model.MaxOutputTokens > 0 {
		maxTokens = *r.Model.MaxOutputTokens
	}
	if s.MaxOutput > 0 && maxTokens > s.MaxOutput {
		maxTokens = s.MaxOutput
	}
	p.MaxTokens = maxTokens
	p.Betas = append(p.Betas, s.Betas...)
	thinking(&p, r.Model.ReasoningEffort, s, maxTokens)
	return p, nil
}

func cacheControl(ttl string) anthropic.BetaCacheControlEphemeralParam {
	cc := anthropic.NewBetaCacheControlEphemeralParam()
	switch ttl {
	case "1h":
		cc.TTL = anthropic.BetaCacheControlEphemeralTTLTTL1h
	case "5m":
		cc.TTL = anthropic.BetaCacheControlEphemeralTTLTTL5m
	}
	return cc
}

func thinking(p *anthropic.BetaMessageNewParams, effort ullm.ReasoningEffort, s ModelSpec, maxTokens int64) {
	switch s.Thinking {
	case "budget":
		if effort == "" {
			return
		}
		budget := min(budgets[effort], maxTokens-1)
		if budget < 1024 {
			// The API's floor; a cap this low has no room to think.
			return
		}
		p.Thinking = anthropic.BetaThinkingConfigParamUnion{OfEnabled: &anthropic.BetaThinkingConfigEnabledParam{BudgetTokens: budget}}
	default:
		ad := anthropic.BetaThinkingConfigAdaptiveParam{}
		if s.Display != "" {
			ad.Display = anthropic.BetaThinkingConfigAdaptiveDisplay(s.Display)
		}
		if s.Display == "updates" {
			p.Betas = append(p.Betas, anthropic.AnthropicBetaThinkingDisplayUpdates2026_08_18)
		}
		p.Thinking = anthropic.BetaThinkingConfigParamUnion{OfAdaptive: &ad}
	}
	if effort != "" && s.Efforts != nil {
		if e := Clamp(string(effort), s.Efforts); e != "" {
			p.OutputConfig.Effort = anthropic.BetaOutputConfigEffort(e)
		}
	}
}

func toolParam(t ullm.Tool) (anthropic.BetaToolParam, error) {
	schema := t.Parameters
	if schema == nil {
		schema = map[string]any{"type": "object", "properties": map[string]any{}}
	}
	def := map[string]any{"name": t.Name, "input_schema": schema}
	if t.Description != "" {
		def["description"] = t.Description
	}
	// Sorted keys (encoding/json sorts map keys), so the tools block, and
	// the cache prefix it opens, is the same bytes every request.
	b, err := stdjson.Marshal(def)
	if err != nil {
		return anthropic.BetaToolParam{}, fmt.Errorf("messagesapi: tool %q schema: %w", t.Name, err)
	}
	return param.Override[anthropic.BetaToolParam](stdjson.RawMessage(b)), nil
}

// renderable drops the items that render to nothing, before any run is
// formed: two user runs separated only by an empty assistant run are one
// message on the wire, and the late-result rule must see them merged too.
func renderable(in []ullm.Item) ([]ullm.Item, error) {
	out := make([]ullm.Item, 0, len(in))
	for i, it := range in {
		switch d := it.Data.(type) {
		case ullm.Message:
			switch d.Role {
			case ullm.RoleSystem:
				if strings.TrimSpace(d.Text) == "" {
					continue
				}
				// Only Input[0] is the system prompt; a later system
				// message is context the model should read in place.
				it.Data = ullm.Message{Role: ullm.RoleUser, Text: "<context-update>\n" + d.Text + "\n</context-update>"}
			default:
				if strings.TrimSpace(d.Text) == "" {
					continue
				}
			}
		case ullm.Reasoning:
			if _, ok := reasoningBlock(d.Raw); !ok {
				continue
			}
		case ullm.ToolCall, ullm.ToolResult:
		default:
			return nil, fmt.Errorf("messagesapi: input item %d has unsupported type %q", i, it.Type)
		}
		out = append(out, it)
	}
	return out, nil
}

// rawBlock is a thinking or redacted_thinking block as Decode stored it.
type rawBlock struct {
	Type      string `json:"type"`
	Thinking  string `json:"thinking,omitzero"`
	Signature string `json:"signature,omitzero"`
	Data      string `json:"data,omitzero"`
}

// reasoningBlock re-encodes a stored reasoning item through the SDK
// types. Anything but a signed thinking block or a redacted one is
// dropped: an unsigned block is a 400, and another provider's reasoning
// never reaches here (the envelope strips it).
func reasoningBlock(raw jsontext.Value) (anthropic.BetaContentBlockParamUnion, bool) {
	var b rawBlock
	if len(raw) == 0 || json.Unmarshal(raw, &b) != nil {
		return anthropic.BetaContentBlockParamUnion{}, false
	}
	switch {
	case b.Type == "thinking" && b.Signature != "":
		return anthropic.BetaContentBlockParamUnion{OfThinking: &anthropic.BetaThinkingBlockParam{Thinking: b.Thinking, Signature: b.Signature}}, true
	case b.Type == "redacted_thinking" && b.Data != "":
		return anthropic.BetaContentBlockParamUnion{OfRedactedThinking: &anthropic.BetaRedactedThinkingBlockParam{Data: b.Data}}, true
	}
	return anthropic.BetaContentBlockParamUnion{}, false
}

// messages groups items into alternating messages.
func messages(items []ullm.Item) ([]anthropic.BetaMessageParam, error) {
	late := wrap.Late(items)
	names := wrap.CallNames(items)
	var out []anthropic.BetaMessageParam
	var calls []string // tool_use ids of the last assistant message
	for i := 0; i < len(items); {
		j := i
		assistant := wrap.AssistantSide(items[i])
		for j < len(items) && wrap.AssistantSide(items[j]) == assistant {
			j++
		}
		if assistant {
			msg, ids := assistantMessage(items[i:j])
			out = append(out, msg)
			calls = ids
		} else {
			out = append(out, userMessage(items[i:j], late[i:j], names, calls))
			calls = nil
		}
		i = j
	}
	return out, nil
}

func assistantMessage(run []ullm.Item) (anthropic.BetaMessageParam, []string) {
	msg := anthropic.BetaMessageParam{Role: anthropic.BetaMessageParamRoleAssistant}
	var ids []string
	for _, it := range run {
		switch d := it.Data.(type) {
		case ullm.Reasoning:
			b, _ := reasoningBlock(d.Raw)
			msg.Content = append(msg.Content, b)
		case ullm.Message:
			msg.Content = append(msg.Content, anthropic.BetaContentBlockParamUnion{OfText: &anthropic.BetaTextBlockParam{Text: d.Text}})
		case ullm.ToolCall:
			msg.Content = append(msg.Content, anthropic.BetaContentBlockParamUnion{OfToolUse: &anthropic.BetaToolUseBlockParam{
				ID:    d.CallID,
				Name:  d.Name,
				Input: arguments(d.Arguments),
			}})
			ids = append(ids, d.CallID)
		}
	}
	return msg, ids
}

// arguments is the tool_use input, byte for byte what the model wrote:
// key order is part of what it generated, and a round trip through a
// map would reorder it and change the cache prefix.
func arguments(args string) stdjson.RawMessage {
	v := jsontext.Value(args)
	if v.Kind() == jsontext.KindBeginObject && v.IsValid() {
		return stdjson.RawMessage(args)
	}
	b, _ := stdjson.Marshal(map[string]string{"invalid_arguments": args})
	return stdjson.RawMessage(b)
}

// userMessage renders one user-side run. Native results for the calls
// of the assistant message before it come first, then a synthesized
// result for any call with none (all results for one assistant message
// in ONE user message: splitting them teaches Claude to stop calling in
// parallel), then everything else at its own position.
func userMessage(run []ullm.Item, late []bool, names map[string]string, calls []string) anthropic.BetaMessageParam {
	msg := anthropic.BetaMessageParam{Role: anthropic.BetaMessageParamRoleUser}
	answered := map[string]bool{}
	for k, it := range run {
		res, ok := it.Data.(ullm.ToolResult)
		if !ok || late[k] {
			continue
		}
		answered[res.CallID] = true
		msg.Content = append(msg.Content, anthropic.BetaContentBlockParamUnion{OfToolResult: toolResult(res)})
	}
	for _, id := range calls {
		if !answered[id] {
			msg.Content = append(msg.Content, anthropic.BetaContentBlockParamUnion{OfToolResult: &anthropic.BetaToolResultBlockParam{
				ToolUseID: id,
				IsError:   anthropic.Bool(true),
				Content:   []anthropic.BetaToolResultBlockParamContentUnion{{OfText: &anthropic.BetaTextBlockParam{Text: missingResult}}},
			}})
		}
	}
	for k, it := range run {
		switch d := it.Data.(type) {
		case ullm.Message:
			msg.Content = append(msg.Content, textBlock(d.Text))
		case ullm.ToolResult:
			if !late[k] {
				continue
			}
			var texts []string
			var images []anthropic.BetaContentBlockParamUnion
			for _, o := range d.Output {
				if o.Kind == ullm.ToolResultImage {
					if img, ok := imageBlock(o.Value); ok {
						images = append(images, anthropic.BetaContentBlockParamUnion{OfImage: img})
					} else {
						texts = append(texts, unsupportedImage(o.Value))
					}
					continue
				}
				texts = append(texts, o.Value)
			}
			msg.Content = append(msg.Content, textBlock(wrap.LateText(d.CallID, names[d.CallID], strings.Join(texts, "\n"))))
			msg.Content = append(msg.Content, images...)
		}
	}
	return msg
}

func textBlock(s string) anthropic.BetaContentBlockParamUnion {
	return anthropic.BetaContentBlockParamUnion{OfText: &anthropic.BetaTextBlockParam{Text: s}}
}

func toolResult(res ullm.ToolResult) *anthropic.BetaToolResultBlockParam {
	out := &anthropic.BetaToolResultBlockParam{ToolUseID: res.CallID}
	for _, o := range res.Output {
		switch o.Kind {
		case ullm.ToolResultImage:
			if img, ok := imageBlock(o.Value); ok {
				out.Content = append(out.Content, anthropic.BetaToolResultBlockParamContentUnion{OfImage: img})
			} else {
				out.Content = append(out.Content, anthropic.BetaToolResultBlockParamContentUnion{OfText: &anthropic.BetaTextBlockParam{Text: unsupportedImage(o.Value)}})
			}
		default:
			// An empty text block is a 400; an empty result is a
			// tool_result with no content, which the API accepts.
			if o.Value != "" {
				out.Content = append(out.Content, anthropic.BetaToolResultBlockParamContentUnion{OfText: &anthropic.BetaTextBlockParam{Text: o.Value}})
			}
		}
	}
	return out
}

// imageBlock reads a data URL. Only png and jpeg are sent: they are
// what view_image produces, and anything else might not be accepted.
func imageBlock(value string) (*anthropic.BetaImageBlockParam, bool) {
	mime, data, ok := dataURL(value)
	if !ok {
		return nil, false
	}
	var mt anthropic.BetaBase64ImageSourceMediaType
	switch mime {
	case "image/png":
		mt = anthropic.BetaBase64ImageSourceMediaTypeImagePNG
	case "image/jpeg":
		mt = anthropic.BetaBase64ImageSourceMediaTypeImageJPEG
	default:
		return nil, false
	}
	return &anthropic.BetaImageBlockParam{Source: anthropic.BetaImageBlockParamSourceUnion{
		OfBase64: &anthropic.BetaBase64ImageSourceParam{Data: data, MediaType: mt},
	}}, true
}

func dataURL(v string) (mime, data string, ok bool) {
	rest, ok := strings.CutPrefix(v, "data:")
	if !ok {
		return "", "", false
	}
	head, data, ok := strings.Cut(rest, ",")
	if !ok {
		return "", "", false
	}
	mime, enc, _ := strings.Cut(head, ";")
	if enc != "base64" {
		return mime, "", false
	}
	return mime, data, true
}

func unsupportedImage(v string) string {
	mime, _, _ := dataURL(v)
	if mime == "" {
		mime = "image"
	}
	return "[image omitted: unsupported " + mime + "]"
}

// placeMarkers puts B2 on the last block of the user message before the
// last assistant message (the previous request's final block, so a read
// is guaranteed whatever the 20-block lookback finds) and B3 on the last
// block. B1 is on the system block. Explicit markers, because legacy
// Bedrock rejects the top-level field.
func placeMarkers(msgs []anthropic.BetaMessageParam, cc anthropic.BetaCacheControlEphemeralParam) {
	last := len(msgs) - 1
	setCache(&msgs[last], cc)
	for i := last - 1; i >= 0; i-- {
		if msgs[i].Role == anthropic.BetaMessageParamRoleAssistant {
			if i > 0 {
				setCache(&msgs[i-1], cc)
			}
			return
		}
	}
}

func setCache(m *anthropic.BetaMessageParam, cc anthropic.BetaCacheControlEphemeralParam) {
	if len(m.Content) == 0 {
		return
	}
	b := &m.Content[len(m.Content)-1]
	switch {
	case b.OfText != nil:
		t := *b.OfText
		t.CacheControl = cc
		b.OfText = &t
	case b.OfImage != nil:
		t := *b.OfImage
		t.CacheControl = cc
		b.OfImage = &t
	case b.OfToolResult != nil:
		t := *b.OfToolResult
		t.CacheControl = cc
		b.OfToolResult = &t
	}
}
