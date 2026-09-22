package prompt

import (
	"encoding/json/jsontext"
	"fmt"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"pgregory.net/rapid"
)

func sample() Parts {
	return Parts{
		Preamble:     Preamble(0),
		Env:          Env("/work", time.Date(2026, 9, 22, 12, 0, 0, 0, time.UTC)),
		Guidance:     "GUIDANCE",
		Ask:          AskSection,
		SessionStart: "HOOK CONTEXT",
		Context: []Part{
			{"/home/me/.bough/projects/p/MEMORY.md", "# Context: MEMORY.md\nremember x\n"},
			{"AGENTS.md", "# Context: AGENTS.md\nbuild with make\n"},
		},
		Sections: []Part{{"rules", "RULES SECTION"}, {"mcp", "MCP SECTION"}},
		Skills: []Skill{
			{"zeta", "Last.", "/s/zeta/SKILL.md"},
			{"alpha", "Use when\n  alpha things   happen.", "/s/alpha/SKILL.md"},
		},
		Schema: "SCHEMA",
	}
}

// Compose follows §11.1's order, sorts what it must sort itself, and
// leaves no gap for an empty part: two sessions with the same parts send
// the same system bytes, which is what keeps the prompt cache warm.
func TestComposeOrderAndDeterminism(t *testing.T) {
	t.Parallel()
	p := sample()
	got := Compose(p)
	order := []string{"You are bough", "Environment:", "GUIDANCE", "`ask` blocks", "HOOK CONTEXT",
		"# Context: MEMORY.md", "# Context: AGENTS.md", "MCP SECTION", "RULES SECTION", "Skills:", "SCHEMA"}
	at := -1
	for _, w := range order {
		i := strings.Index(got, w)
		if i <= at {
			t.Fatalf("%q out of order (at %d, previous at %d):\n%s", w, i, at, got)
		}
		at = i
	}
	if !strings.Contains(got, "- alpha: Use when alpha things happen. (/s/alpha/SKILL.md)\n- zeta: Last. (/s/zeta/SKILL.md)") {
		t.Fatalf("skills catalogue not sorted and single-lined:\n%s", got)
	}
	shuffled := sample()
	shuffled.Sections = []Part{p.Sections[1], p.Sections[0]}
	shuffled.Skills = []Skill{p.Skills[1], p.Skills[0]}
	if Compose(shuffled) != got || Hash(shuffled) != Hash(p) {
		t.Fatal("the same parts in another order composed differently")
	}
	if strings.Contains(Compose(Parts{Preamble: "A", Schema: "B"}), "\n\n\n") || Compose(Parts{Preamble: "A", Schema: "B"}) != "A\n\nB" {
		t.Fatalf("empty parts left a gap: %q", Compose(Parts{Preamble: "A", Schema: "B"}))
	}
	if len(Hash(p)) != 64 {
		t.Fatalf("hash %q is not a sha256", Hash(p))
	}
}

// The heartbeat line is there only when heartbeats are sent, and
// engine.md carries no harness branding.
func TestPreamble(t *testing.T) {
	t.Parallel()
	if strings.Contains(Preamble(0), "heartbeat message") {
		t.Fatal("heartbeat line without heartbeats")
	}
	if !strings.Contains(Preamble(90*time.Second), "for 1m30s, a heartbeat") {
		t.Fatalf("heartbeat line missing: %s", Preamble(90*time.Second))
	}
	for _, banned := range []string{"Unreal", "unreal", "tools."} {
		if strings.Contains(Preamble(0), banned) {
			t.Fatalf("engine.md mentions %q", banned)
		}
	}
	for _, must := range []string{"<tool_result call_id=", "background: true", "<context-update>", "Scope:", "Verification:"} {
		if !strings.Contains(Preamble(0), must) {
			t.Fatalf("engine.md lacks %q", must)
		}
	}
}

func TestReminder(t *testing.T) {
	t.Parallel()
	prev := sample()

	if text, changed := Reminder(prev, prev); text != "" || changed != nil {
		t.Fatalf("no change gave %q %v", text, changed)
	}

	// The parts that are the session's for good never produce a reminder:
	// the engine reads them lazily without Preamble, Guidance or the
	// one-shot session-start context.
	now := sample()
	now.Preamble, now.Guidance, now.SessionStart = "", "", ""
	if text, _ := Reminder(prev, now); text != "" {
		t.Fatalf("frozen-only parts reminded: %q", text)
	}

	now = sample()
	now.Context[1].Text = "# Context: AGENTS.md\nbuild with bazel\n"
	now.Sections = []Part{{"rules", "RULES SECTION"}, {"orb", "ORB READY"}}
	now.Context = append(now.Context, Part{"CLAUDE.md", "# Context: CLAUDE.md\nnew file\n"})
	text, changed := Reminder(prev, now)
	want := []string{"AGENTS.md", "CLAUDE.md", "prompt-section orb", "prompt-section mcp"}
	if !reflect.DeepEqual(changed, want) {
		t.Fatalf("changed = %v, want %v", changed, want)
	}
	if !strings.HasPrefix(text, `<context-update source="AGENTS.md, CLAUDE.md, prompt-section orb, prompt-section mcp">`+"\n") ||
		!strings.HasSuffix(text, "\n</context-update>") {
		t.Fatalf("envelope:\n%s", text)
	}
	for _, w := range []string{"build with bazel", "new file", "ORB READY", "(prompt-section mcp no longer applies"} {
		if !strings.Contains(text, w) {
			t.Fatalf("reminder lacks %q:\n%s", w, text)
		}
	}
	for _, not := range []string{"remember x", "RULES SECTION", "build with make"} {
		if strings.Contains(text, not) {
			t.Fatalf("reminder repeats unchanged %q:\n%s", not, text)
		}
	}

	// A new skill resends the catalogue whole; a new day, the environment.
	now = sample()
	now.Skills = append(now.Skills, Skill{"beta", "b", "/s/beta/SKILL.md"})
	now.Env = Env("/work", time.Date(2026, 9, 23, 0, 0, 0, 0, time.UTC))
	text, changed = Reminder(prev, now)
	if !reflect.DeepEqual(changed, []string{"environment", "skills"}) || !strings.Contains(text, "- alpha:") || !strings.Contains(text, "- beta: b") || !strings.Contains(text, "Date: 2026-09-23") {
		t.Fatalf("changed %v:\n%s", changed, text)
	}
}

func TestWrapSubstitutes(t *testing.T) {
	t.Parallel()
	b := Wrap(contextbuilder.NewBuilder(), "FROZEN", Placeholder)
	b.SetSystemPrompt("ignored")
	input(t, b, 1)
	b.AddModelResponse(ullm.Response{Output: []ullm.Item{call("c1"), call("c2")}})
	b.AddToolResult("c1", nil, true)
	b.AddToolResult("c2", text("done c2"), false)
	res, err := b.Build()
	if err != nil {
		t.Fatal(err)
	}
	in := res.Request.Input
	if m := in[0].Data.(ullm.Message); m.Role != ullm.RoleSystem || m.Text != "FROZEN" {
		t.Fatalf("Input[0] = %+v", m)
	}
	if r := in[len(in)-2].Data.(ullm.ToolResult); r.CallID != "c1" || r.Output[0].Value != Placeholder {
		t.Fatalf("running result = %+v", r)
	}
	if r := in[len(in)-1].Data.(ullm.ToolResult); r.Output[0].Value != "done c2" {
		t.Fatalf("final result changed: %+v", r)
	}
	// The final for c1 replaces the placeholder in the staged suffix, as
	// the inner builder does: Wrap keeps no state of its own.
	b.AddToolResult("c1", text("done c1"), false)
	res, _ = b.Build()
	for _, it := range res.Request.Input {
		if r, ok := it.Data.(ullm.ToolResult); ok && r.Output[0].Value == Placeholder {
			t.Fatalf("placeholder survived the final: %+v", res.Request.Input)
		}
	}
}

// Under any sequence of builder operations the coordinator can make,
// Wrap's request is the inner request with exactly the two substitutions,
// and with a Commit after each Build (the coordinator's order) request k
// is a prefix of request k+1.
func TestWrapIsAppendOnly(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		inner := contextbuilder.NewBuilder()
		b := Wrap(inner, "FROZEN", Placeholder)
		inputs, calls := 0, 0
		var pending, running []string
		var prev []ullm.Item
		steps := rapid.IntRange(1, 40).Draw(rt, "steps")
		for s := 0; s < steps; s++ {
			switch rapid.IntRange(0, 4).Draw(rt, "op") {
			case 0:
				inputs++
				input(rt, b, inputs)
			case 1:
				n := rapid.IntRange(0, 3).Draw(rt, "calls")
				out := []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: fmt.Sprint("reply ", s)}}}
				for range n {
					calls++
					id := fmt.Sprint("c", calls)
					out = append(out, call(id))
					pending = append(pending, id)
				}
				b.AddModelResponse(ullm.Response{Output: out})
			case 2:
				if len(pending) == 0 {
					continue
				}
				id := pending[0]
				pending = pending[1:]
				if rapid.Bool().Draw(rt, "runs") {
					b.AddToolResult(id, nil, true)
					running = append(running, id)
				} else {
					b.AddToolResult(id, text("out "+id), false)
				}
			case 3:
				if len(running) == 0 {
					continue
				}
				i := rapid.IntRange(0, len(running)-1).Draw(rt, "final")
				id := running[i]
				running = append(running[:i:i], running[i+1:]...)
				b.AddToolResult(id, text("final "+id), false)
			case 4:
				got, err := b.Build()
				if err != nil {
					rt.Fatal(err)
				}
				want, _ := inner.Build()
				if len(got.Request.Input) != len(want.Request.Input) {
					rt.Fatalf("Wrap changed the item count: %d vs %d", len(got.Request.Input), len(want.Request.Input))
				}
				for i, it := range got.Request.Input {
					w := want.Request.Input[i]
					switch r, _ := w.Data.(ullm.ToolResult); {
					case i == 0:
						if it.Data.(ullm.Message).Text != "FROZEN" {
							rt.Fatalf("Input[0] = %+v", it)
						}
					case w.Type == ullm.ItemToolResult && r.Output[0].Value == contextbuilder.ToolCallRunningPayload:
						if it.Data.(ullm.ToolResult).Output[0].Value != Placeholder || it.Data.(ullm.ToolResult).CallID != r.CallID {
							rt.Fatalf("item %d not substituted: %+v", i, it)
						}
					default:
						if !reflect.DeepEqual(it, w) {
							rt.Fatalf("item %d changed: %+v vs %+v", i, it, w)
						}
					}
				}
				if prev != nil && (len(got.Request.Input) < len(prev) || !reflect.DeepEqual(prev, got.Request.Input[:len(prev)])) {
					rt.Fatalf("request is not an append to the previous one:\n was %+v\n now %+v", prev, got.Request.Input)
				}
				prev = got.Request.Input
				b.Commit()
			}
		}
	})
}

type fataler interface{ Fatal(...any) }

func input(t fataler, b contextbuilder.Builder, n int) {
	err := b.AddExternalInput(inbox.Input{ID: inbox.ID(fmt.Sprint(n)), Kind: inbox.InputExternal,
		Payload: jsontext.Value(strconv.Quote(fmt.Sprint("input ", n)))})
	if err != nil {
		t.Fatal(err)
	}
}

func call(id string) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id, Name: "bash", Arguments: `{"command":"ls"}`}}
}

func text(s string) []ullm.ToolResultOutput {
	return []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: s}}
}
