package echo_test

import (
	"strings"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/echo"
)

func msg(role ullm.Role, s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: role, Text: s}}
}

func respond(t *testing.T, in ...ullm.Item) (ullm.Response, []agentllm.Delta) {
	t.Helper()
	var ds []agentllm.Delta
	a := echo.New(agentllm.Options{Sink: func(d agentllm.Delta) { ds = append(ds, d) }})
	r, err := a.Respond(agentllm.WithSeq(t.Context(), 5), ullm.Request{Input: in}, ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	return r, ds
}

func only(t *testing.T, r ullm.Response) any {
	t.Helper()
	if len(r.Output) != 1 {
		t.Fatalf("output = %+v", r.Output)
	}
	return r.Output[0].Data
}

// The rules apply in the design's order (§14.2), first match wins.
func TestEchoRules(t *testing.T) {
	t.Parallel()
	sys := msg(ullm.RoleSystem, "THE PROMPT")
	for _, tc := range []struct {
		name string
		in   []ullm.Item
		want string // "text:<text>" or "call:<name> <args>"
	}{
		{"system", []ullm.Item{sys, msg(ullm.RoleUser, "show SYSTEM! and CODE!")}, "text:THE PROMPT"},
		{"result", []ullm.Item{sys, msg(ullm.RoleUser, "CODE!"),
			{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "c", Name: "bash", Arguments: "{}"}},
			{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "c", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "hi from codemode\nsecond line"}}}}},
			"text:ran: hi from codemode"},
		{"code", []ullm.Item{sys, msg(ullm.RoleUser, "say CODE! please")}, `call:bash {"command":"echo hi from codemode"}`},
		{"slow", []ullm.Item{sys, msg(ullm.RoleUser, "SLOW!")}, `call:bash {"command":"sleep 3; echo slow done"}`},
		{"ask", []ullm.Item{sys, msg(ullm.RoleUser, "ASK!")}, `call:ask {"question":"echo asks?","options":["yes","no"]}`},
		{"spawn", []ullm.Item{sys, msg(ullm.RoleUser, "SPAWN!")}, `call:spawn {"task":"say hi"}`},
		{"echo", []ullm.Item{sys, msg(ullm.RoleUser, "hello there")}, "text:echo: hello there"},
	} {
		r, ds := respond(t, tc.in...)
		var got string
		switch d := only(t, r).(type) {
		case ullm.Message:
			got = "text:" + d.Text
			var streamed strings.Builder
			for _, x := range ds {
				if x.Kind != agentllm.DeltaText || x.Seq != 5 {
					t.Errorf("%s: delta %+v", tc.name, x)
				}
				streamed.WriteString(x.Text)
			}
			if streamed.String() != d.Text {
				t.Errorf("%s: streamed %q, replied %q", tc.name, streamed.String(), d.Text)
			}
		case ullm.ToolCall:
			got = "call:" + d.Name + " " + d.Arguments
			if d.CallID == "" || len(ds) != 1 || ds[0].Kind != agentllm.DeltaToolStart || ds[0].CallID != d.CallID {
				t.Errorf("%s: call %+v deltas %+v", tc.name, d, ds)
			}
		}
		if got != tc.want {
			t.Errorf("%s: got %q, want %q", tc.name, got, tc.want)
		}
	}
}

// Call ids are the input length, so a resumed session replays the same.
func TestEchoCallIDsAreStable(t *testing.T) {
	t.Parallel()
	r1, _ := respond(t, msg(ullm.RoleSystem, "s"), msg(ullm.RoleUser, "CODE!"))
	r2, _ := respond(t, msg(ullm.RoleSystem, "s"), msg(ullm.RoleUser, "CODE!"))
	if r1.Output[0].Data.(ullm.ToolCall).CallID != "echo_2" || r2.Output[0].Data.(ullm.ToolCall).CallID != "echo_2" {
		t.Errorf("ids %+v %+v", r1.Output, r2.Output)
	}
	if a := echo.New(); a.Provider() != "echo" || a.Model() != "echo" {
		t.Error("identity")
	}
}
