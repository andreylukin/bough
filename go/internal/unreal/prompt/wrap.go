package prompt

import (
	"slices"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

// Wrap is inner with two substitutions made at Build: Input[0] carries
// system, bough's frozen prompt, in place of the harness preamble plus
// whatever SetSystemPrompt set; and a result that is only the harness's
// running-call payload carries placeholder instead. Both are constant
// for a coordinator, so request k+1 still starts with request k's bytes.
func Wrap(inner contextbuilder.Builder, system, placeholder string) contextbuilder.Builder {
	return &wrapped{Builder: inner, system: system, placeholder: placeholder}
}

type wrapped struct {
	contextbuilder.Builder
	system, placeholder string
}

func (w *wrapped) Build() (contextbuilder.Result, error) {
	res, err := w.Builder.Build()
	if err != nil {
		return res, err
	}
	in := slices.Clone(res.Request.Input)
	if len(in) > 0 {
		in[0] = ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: w.system}}
	}
	for i, it := range in {
		r, ok := it.Data.(ullm.ToolResult)
		if ok && len(r.Output) == 1 && r.Output[0].Kind == ullm.ToolResultText && r.Output[0].Value == contextbuilder.ToolCallRunningPayload {
			in[i] = ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{
				CallID: r.CallID,
				Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: w.placeholder}},
			}}
		}
	}
	res.Request.Input = in
	return res, nil
}
