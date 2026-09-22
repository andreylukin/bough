package toolreg

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"strings"
	"testing"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/tool"
	"github.com/unreallabsai/unreal-agent/harness/tool/viewimage"

	"github.com/andreylukin/bough/internal/agenttools"
)

// submitter is a tool.Context that records what a translator submits.
type submitter struct{ specs []operation.Spec }

func (s *submitter) Submit(spec operation.Spec) operation.ID {
	s.specs = append(s.specs, spec)
	return operation.ID("op-" + string(rune('0'+len(s.specs))))
}

func noop(context.Context, agenttools.Call) (agenttools.Result, error) {
	return agenttools.Result{}, nil
}

func tools() []agenttools.Tool {
	return []agenttools.Tool{
		{Name: "write", Description: "write a file", Schema: agenttools.Object([]string{"path"}, map[string]any{"path": agenttools.Prop("string", "p")}), Call: noop},
		{Name: "bash", Description: "run", Schema: agenttools.Object([]string{"command"}, map[string]any{"command": agenttools.Prop("string", "c")}), Call: noop,
			Detail: func(a json.RawMessage) string { return "bash-detail" }},
	}
}

func TestDefinitionsSortedWithViewImage(t *testing.T) {
	t.Parallel()
	r := New(Config{Tools: tools(), ViewImage: viewimage.New(viewimage.Config{Directory: t.TempDir()})})
	var names []string
	for _, d := range r.StaticDefinitions() {
		names = append(names, d.Tool.Name)
		if d.Tool.Type != ullm.ToolFunction || d.Tool.Parameters["type"] != "object" {
			t.Fatalf("definition %s = %#v", d.Tool.Name, d.Tool)
		}
	}
	if got := strings.Join(names, ","); got != "bash,view_image,write" {
		t.Fatalf("definitions = %s", got)
	}
	if n := len(New(Config{Tools: tools()}).StaticDefinitions()); n != 2 {
		t.Fatalf("no ViewImage: %d definitions, want 2", n)
	}
}

func TestResolveAlwaysTrue(t *testing.T) {
	t.Parallel()
	r := New(Config{Tools: tools()})
	for _, name := range []string{"bash", "write", "retired", "Made Up!", ViewImageName} {
		tr, ok := r.Resolve(name)
		if !ok || tr == nil {
			t.Fatalf("Resolve(%q) = %v, %v", name, tr, ok)
		}
	}
	tr, _ := r.Resolve("retired")
	var s submitter
	st := tr.Translate(&s, ullm.ToolCall{CallID: "c1", Name: "retired", Arguments: "{}"})
	if st.Error != `tool "retired" is not available in this session` || len(s.specs) != 0 {
		t.Fatalf("tombstone Translate = %#v, submitted %d", st, len(s.specs))
	}
	res, err := tr.TranslateResult("c1", st, nil)
	if err != nil || res.Output[0].Value != "Error: "+st.Error {
		t.Fatalf("tombstone result = %#v, %v", res, err)
	}
}

func TestTranslateSubmitsOneBoughCall(t *testing.T) {
	t.Parallel()
	r := New(Config{Tools: tools(), MaxOutput: 1234})
	tr, _ := r.Resolve("bash")
	var s submitter
	st := tr.Translate(&s, ullm.ToolCall{CallID: "call_1", Name: "bash", Arguments: `{"command": "ls",  "z": 1, "a": 2}`})
	if st.Error != "" || len(st.WaitingFor) != 1 || len(s.specs) != 1 {
		t.Fatalf("status = %#v, specs = %d", st, len(s.specs))
	}
	spec := s.specs[0]
	if spec.Type != operation.TypeRemoteJob || spec.MaxOutputLength != 1234 {
		t.Fatalf("spec = %#v", spec)
	}
	op := operation.Operation{ID: "x", Type: spec.Type, Version: spec.Version, MaxOutputLength: spec.MaxOutputLength, Status: operation.StatusReady, State: spec.State}
	p, ok := DecodePlan(op)
	if !ok || p.Call != "call_1" || p.Tool != "bash" {
		t.Fatalf("plan = %#v, %v", p, ok)
	}
	// Key order is part of what the model generated: never re-sorted.
	if string(p.Args) != `{"command":"ls","z":1,"a":2}` {
		t.Fatalf("args = %s", p.Args)
	}
	// MaxOutput beyond the harness cap is clamped, or the op is invalid.
	tr, _ = New(Config{Tools: tools(), MaxOutput: 5_000_000}).Resolve("bash")
	s = submitter{}
	tr.Translate(&s, ullm.ToolCall{CallID: "c", Name: "bash", Arguments: `{}`})
	if s.specs[0].MaxOutputLength != operation.MaxOutputLength {
		t.Fatalf("limit = %d", s.specs[0].MaxOutputLength)
	}
}

func TestTranslateRejectsNonObjectArgs(t *testing.T) {
	t.Parallel()
	tr, _ := New(Config{Tools: tools()}).Resolve("write")
	for _, bad := range []string{`[1]`, `"x"`, `3`, `null`, `{broken`} {
		var s submitter
		st := tr.Translate(&s, ullm.ToolCall{CallID: "c", Name: "write", Arguments: bad})
		if st.Error != "arguments must be a JSON object" || len(s.specs) != 0 {
			t.Fatalf("args %s: status %#v", bad, st)
		}
	}
	var s submitter
	if st := tr.Translate(&s, ullm.ToolCall{CallID: "c", Name: "write", Arguments: ""}); st.Error != "" {
		t.Fatalf("empty args rejected: %#v", st)
	}
}

func remoteOp(t *testing.T, status operation.Status, st operation.RemoteJobState) operation.Operation {
	t.Helper()
	data, _ := json.Marshal(Plan{Call: "c", Tool: "bash", Args: json.RawMessage(`{}`)})
	st.Plan = operation.RemoteJobPlan{Type: PlanType, Version: PlanVersion, Data: jsontext.Value(data)}
	b, err := json.Marshal(st)
	if err != nil {
		t.Fatal(err)
	}
	return operation.Operation{ID: "op", Type: operation.TypeRemoteJob, Version: operation.VersionRemoteJob, MaxOutputLength: 100, Status: status, State: b}
}

func TestRenderByStatus(t *testing.T) {
	t.Parallel()
	hd, _ := json.Marshal(Handle{Detail: "ls", Data: map[string]any{"exit": 0}, MS: 12})
	cancelled, _ := json.Marshal(Handle{Error: "user pressed esc"})
	for _, c := range []struct {
		name     string
		status   tool.CallStatus
		ops      []operation.Operation
		want     string
		terminal bool
	}{
		{"validation error", tool.CallStatus{Error: "bad"}, nil, "Error: bad", true},
		{"no ops", tool.CallStatus{}, nil, "Error: no result was recorded for this call", true},
		{"ready", tool.CallStatus{WaitingFor: []operation.ID{"op"}}, []operation.Operation{remoteOp(t, operation.StatusReady, operation.RemoteJobState{})}, contextbuilder.ToolCallRunningPayload, false},
		{"awaiting", tool.CallStatus{}, []operation.Operation{remoteOp(t, operation.StatusAwaiting, operation.RemoteJobState{})}, contextbuilder.ToolCallRunningPayload, false},
		{"completed", tool.CallStatus{}, []operation.Operation{remoteOp(t, operation.StatusCompleted, operation.RemoteJobState{TerminalResult: "hi", Handle: hd})}, "hi", true},
		{"failed", tool.CallStatus{}, []operation.Operation{remoteOp(t, operation.StatusFailed, operation.RemoteJobState{TerminalError: "boom"})}, "Error: boom", true},
		{"canceled", tool.CallStatus{}, []operation.Operation{remoteOp(t, operation.StatusCanceled, operation.RemoteJobState{Handle: cancelled})}, "Cancelled: user pressed esc", true},
		{"canceled bare", tool.CallStatus{}, []operation.Operation{remoteOp(t, operation.StatusCanceled, operation.RemoteJobState{})}, "Cancelled", true},
		{"garbage state", tool.CallStatus{}, []operation.Operation{{ID: "op", Type: operation.TypeRemoteJob, Version: operation.VersionRemoteJob, MaxOutputLength: 10, Status: operation.StatusCompleted, State: jsontext.Value(`{}`)}}, "", true},
	} {
		text, h, terminal := Render("c", c.status, c.ops)
		if c.name == "garbage state" {
			if !strings.HasPrefix(text, "Error: unreadable call state") || !terminal {
				t.Fatalf("%s: %q %v", c.name, text, terminal)
			}
			continue
		}
		if text != c.want || terminal != c.terminal {
			t.Fatalf("%s: Render = %q, %v; want %q, %v", c.name, text, terminal, c.want, c.terminal)
		}
		if c.name == "completed" && (h.Detail != "ls" || h.MS != 12 || h.Data["exit"] != float64(0)) {
			t.Fatalf("completed handle = %#v", h)
		}
		// Pure: the same inputs give the same bytes, every time.
		again, _, _ := Render("c", c.status, c.ops)
		if again != text {
			t.Fatalf("%s: Render not deterministic", c.name)
		}
	}
}

func TestHash(t *testing.T) {
	t.Parallel()
	a := tools()
	b := []agenttools.Tool{a[1], a[0]}
	if Hash(a) != Hash(b) {
		t.Fatal("Hash depends on order")
	}
	c := tools()
	c[0].Description = "write a whole file"
	if Hash(a) == Hash(c) {
		t.Fatal("Hash ignores the description")
	}
	d := tools()
	d[0].Schema = agenttools.Object(nil, map[string]any{"path": agenttools.Prop("string", "other")})
	if Hash(a) == Hash(d) {
		t.Fatal("Hash ignores the schema")
	}
	// Call and Detail are not what the model sees.
	e := tools()
	e[1].Detail = nil
	if Hash(a) != Hash(e) {
		t.Fatal("Hash depends on Detail")
	}
}

func TestDetail(t *testing.T) {
	t.Parallel()
	d := Detail(tools())
	if got := d("bash", json.RawMessage(`{}`)); got != "bash-detail" {
		t.Fatalf("bash detail = %q", got)
	}
	if got := d("write", json.RawMessage(`{}`)); got != "" {
		t.Fatalf("write detail = %q", got)
	}
	if got := d(ViewImageName, json.RawMessage(`{"path":"a.png"}`)); got != "a.png" {
		t.Fatalf("view_image detail = %q", got)
	}
}
