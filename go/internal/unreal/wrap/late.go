package wrap

import (
	"strconv"
	"strings"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

// AssistantSide reports whether an item belongs to the model's turn.
// Everything else (user and system messages, tool results) is what the
// harness sends back, and consecutive items of one side are one message
// on the Anthropic wire.
func AssistantSide(it ullm.Item) bool {
	switch it.Type {
	case ullm.ItemToolCall, ullm.ItemReasoning:
		return true
	case ullm.ItemMessage:
		m, _ := it.Data.(ullm.Message)
		return m.Role == ullm.RoleAssistant
	}
	return false
}

// Late marks, per index, every ToolResult that cannot be a native tool
// result: a second result for a call id, or one that does not sit in
// the user-side run right after the assistant run holding its call.
//
// The harness commits a "still running" placeholder as a call's first
// result and appends the real one later, so the real one is usually
// late. Deciding by position alone keeps the render a pure function of
// the item list, which is what keeps the transcript append-only across
// requests: nothing already sent is ever rewritten.
//
// A caller that drops items before rendering (an empty assistant text,
// reasoning the provider cannot read) must drop them before calling
// Late, so that runs merge here exactly as they merge on the wire.
func Late(in []ullm.Item) []bool {
	late := make([]bool, len(in))
	seen := map[string]bool{}
	// calls is the call ids of the assistant run before the current
	// user-side run; nil before the first assistant run.
	var calls map[string]bool
	prevAssistant := false
	for i, it := range in {
		if AssistantSide(it) {
			if !prevAssistant {
				calls = map[string]bool{}
			}
			prevAssistant = true
			if c, ok := it.Data.(ullm.ToolCall); ok {
				calls[c.CallID] = true
			}
			continue
		}
		prevAssistant = false
		res, ok := it.Data.(ullm.ToolResult)
		if !ok {
			continue
		}
		first := !seen[res.CallID]
		seen[res.CallID] = true
		late[i] = !first || !calls[res.CallID]
	}
	return late
}

// CallNames maps every call id in the input to its tool name.
func CallNames(in []ullm.Item) map[string]string {
	names := map[string]string{}
	for _, it := range in {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			names[c.CallID] = c.Name
		}
	}
	return names
}

// LateText is the text form of a late result. It is the shape the
// claude-api skill prescribes for a result that cannot be a tool_result
// block: the model reads which call it answers, and earlier copies
// (the placeholder) stay where they are.
func LateText(callID, name, text string) string {
	var b strings.Builder
	b.WriteString("<tool_result call_id=")
	b.WriteString(strconv.Quote(callID))
	b.WriteString(" name=")
	b.WriteString(strconv.Quote(name))
	b.WriteString(">\n")
	b.WriteString(text)
	b.WriteString("\n</tool_result>")
	return b.String()
}
