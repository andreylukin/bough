//go:build !windows

package messagesapi

import (
	stdjson "encoding/json"
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strconv"
	"testing"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"pgregory.net/rapid"

	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/prompt"
)

// wireMessages is each rendered message as JSON with cache_control
// stripped: markers move every request by design; nothing else may.
func wireMessages(t *rapid.T, r ullm.Request) (msgs []map[string]any, raw []string) {
	p, err := Render(r, Spec("claude-opus-5-5"), "1h")
	if err != nil {
		t.Fatalf("render: %v\n%s", err, fake.Render(r))
	}
	b, err := stdjson.Marshal(p.Messages)
	if err != nil {
		t.Fatal(err)
	}
	if err := stdjson.Unmarshal(b, &msgs); err != nil {
		t.Fatal(err)
	}
	for _, m := range msgs {
		for _, blk := range m["content"].([]any) {
			delete(blk.(map[string]any), "cache_control")
		}
		mb, _ := stdjson.Marshal(m)
		raw = append(raw, string(mb))
	}
	return msgs, raw
}

func blocks(m map[string]any) []string {
	var out []string
	for _, b := range m["content"].([]any) {
		bb, _ := stdjson.Marshal(b)
		out = append(out, string(bb))
	}
	return out
}

// The stop-the-line property (§15.1 W1, risk 1). The real contextbuilder
// is driven the way the coordinator drives it, with random responses
// (text, calls, reasoning, nothing), placeholders, finals that arrive
// early or late, and user input at any point. For every pair of
// consecutive requests, with cache_control stripped:
//   - every message of request k but its last is byte-identical in k+1;
//   - the blocks of k's last message prefix the same message in k+1;
//   - every tool_use has exactly one tool_result in the next message;
//   - no render ends on the assistant.
//
// A breach is a rewritten prompt cache, a preserved-thinking 400 on
// Opus 5.5 and Fable 5.1, or a tool_use/tool_result 400.
func TestRenderIsAppendOnly(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		b := prompt.Wrap(contextbuilder.NewBuilder(), "frozen system prompt", prompt.Placeholder)
		b.SetModel(ullm.Model{ID: "claude-opus-5-5"})
		b.AddTool(bash)
		inputs, calls := 0, 0
		addInput := func() {
			inputs++
			payload := jsontext.Value(strconv.Quote(fmt.Sprintf("input %d", inputs)))
			if err := b.AddExternalInput(inbox.Input{ID: inbox.ID(fmt.Sprint(inputs)), Kind: inbox.InputExternal, Payload: payload}); err != nil {
				rt.Fatal(err)
			}
		}
		addInput()
		var pending, running []string
		fresh := true // something user-side arrived since the last response
		var prev []map[string]any
		var prevRaw []string

		requests := rapid.IntRange(1, 10).Draw(rt, "requests")
		for k := 0; k < requests; k++ {
			ops := rapid.IntRange(0, 4).Draw(rt, "ops")
			for op := 0; op < ops || len(pending) > 0; op++ {
				kind := rapid.IntRange(0, 2).Draw(rt, "op")
				switch {
				case kind == 0 && len(pending) > 0:
					i := rapid.IntRange(0, len(pending)-1).Draw(rt, "pending")
					id := pending[i]
					pending = slices.Delete(pending, i, i+1)
					if rapid.Bool().Draw(rt, "runs") {
						b.AddToolResult(id, nil, true)
						running = append(running, id)
					} else {
						b.AddToolResult(id, []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "out " + id}}, false)
					}
				case kind == 1 && len(running) > 0:
					i := rapid.IntRange(0, len(running)-1).Draw(rt, "running")
					id := running[i]
					running = slices.Delete(running, i, i+1)
					b.AddToolResult(id, []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "final " + id}}, false)
				case len(pending) > 0:
					// Every call of the last response gets a first result
					// before the coordinator asks again.
					id := pending[0]
					pending = pending[1:]
					b.AddToolResult(id, []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "out " + id}}, false)
				default:
					addInput()
				}
				fresh = true
			}
			if !fresh {
				addInput()
			}
			built, err := b.Build()
			if err != nil {
				rt.Fatal(err)
			}
			req := built.Request
			msgs, raw := wireMessages(rt, req)
			b.Commit() // the coordinator commits when it records the turn

			if msgs[len(msgs)-1]["role"] != "user" {
				rt.Fatalf("request %d ends on the assistant", k+1)
			}
			for i, m := range msgs {
				if m["role"] != "assistant" {
					continue
				}
				var ids []string
				for _, blk := range m["content"].([]any) {
					bm := blk.(map[string]any)
					if bm["type"] == "tool_use" {
						ids = append(ids, bm["id"].(string))
					}
				}
				for _, id := range ids {
					n := 0
					for _, blk := range msgs[i+1]["content"].([]any) {
						bm := blk.(map[string]any)
						if bm["type"] == "tool_result" && bm["tool_use_id"] == id {
							n++
						}
					}
					if n != 1 {
						rt.Fatalf("request %d: tool_use %s has %d tool_result blocks in the next message", k+1, id, n)
					}
				}
			}
			if prev != nil {
				if len(msgs) < len(prev) {
					rt.Fatalf("request %d has %d messages, request %d had %d", k+1, len(msgs), k, len(prev))
				}
				for i := 0; i < len(prev)-1; i++ {
					if raw[i] != prevRaw[i] {
						rt.Fatalf("request %d rewrote message %d:\n was %s\n now %s", k+1, i, prevRaw[i], raw[i])
					}
				}
				last := len(prev) - 1
				pb, nb := blocks(prev[last]), blocks(msgs[last])
				if prev[last]["role"] != msgs[last]["role"] || len(nb) < len(pb) || !slices.Equal(pb, nb[:len(pb)]) {
					rt.Fatalf("request %d: the blocks of the previous last message are not a prefix:\n was %v\n now %v", k+1, pb, nb)
				}
			}
			prev, prevRaw = msgs, raw

			// The response: nothing (a muted Gate reply), text, or
			// reasoning and calls.
			var out []ullm.Item
			switch rapid.IntRange(0, 3).Draw(rt, "response") {
			case 1:
				out = append(out, fake.Text(fmt.Sprintf("reply %d", k)))
			case 2:
				out = append(out, fake.Think(fmt.Sprintf("thought %d", k)), fake.Text(fmt.Sprintf("working %d", k)))
				fallthrough
			case 3:
				for n := rapid.IntRange(1, 3).Draw(rt, "calls"); n > 0; n-- {
					calls++
					id := fmt.Sprintf("toolu_%d", calls)
					out = append(out, fake.Call(id, "bash", `{"command":"echo `+id+`"}`))
					pending = append(pending, id)
				}
			}
			b.AddModelResponse(ullm.Response{Output: out})
			fresh = false
		}
	})
}
