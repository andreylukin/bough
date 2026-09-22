package project

import (
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/tool"
)

var update = flag.Bool("update", false, "rewrite the golden files")

var t0 = time.Date(2026, 9, 22, 12, 0, 0, 0, time.UTC)

// step is one input to the projector: exactly one field is set.
type step struct {
	item     *sessionstore.Item
	meta     *Meta
	delta    *agentllm.Delta
	progress [2]string
	adopt    *struct {
		call string
		job  int
	}
}

// testRender is §8.2's Render table, local until W3's toolreg lands: the
// projector only needs its text, Handle and terminal flag.
func testRender(callID string, st tool.CallStatus, ops []operation.Operation) (string, toolreg.Handle, bool) {
	var h toolreg.Handle
	var texts []string
	for _, op := range ops {
		s, err := operation.DecodeRemoteJobState(op)
		if err != nil {
			return "", h, false
		}
		if len(s.Handle) > 0 {
			_ = json.Unmarshal(s.Handle, &h)
		}
		switch op.Status {
		case operation.StatusCompleted:
			texts = append(texts, s.TerminalResult)
		case operation.StatusFailed:
			texts = append(texts, "Error: "+s.TerminalError)
		case operation.StatusCanceled:
			texts = append(texts, "Cancelled: "+h.Error)
		default:
			return "running", h, false
		}
	}
	return strings.Join(texts, "\n"), h, true
}

func testDetail(name string, args json.RawMessage) string {
	var a map[string]any
	_ = json.Unmarshal(args, &a)
	for _, k := range []string{"command", "path"} {
		if s, ok := a[k].(string); ok {
			return strings.SplitN(s, "\n", 2)[0]
		}
	}
	return ""
}

func cfg() Config {
	return Config{Detail: testDetail, Render: testRender, JS: "run_js"}
}

type builder struct {
	seq uint64
	ms  int
}

func (b *builder) item(kind sessionstore.ItemKind, data any) step {
	b.seq++
	b.ms += 250
	return step{item: &sessionstore.Item{Sequence: sessionstore.Sequence(b.seq), RecordedAt: t0.Add(time.Duration(b.ms) * time.Millisecond), Kind: kind, Data: data}}
}

func (b *builder) wait(ms int) { b.ms += ms }

func (b *builder) turn(id string) step {
	return b.item(sessionstore.ItemTurn, session.Turn{ID: session.TurnID(id), Type: session.TurnRegular})
}

func (b *builder) response(id string, stop ullm.StopReason, items ...ullm.Item) step {
	if stop == "" {
		stop = ullm.StopComplete
	}
	return b.item(sessionstore.ItemModelResponse, sessionstore.ModelResponse{TurnID: "t", Response: ullm.Response{ID: id, Stop: stop, Output: items}})
}

func text(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: s}}
}

func reasoning(s ...string) ullm.Item {
	return ullm.Item{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Summary: s}}
}

func toolCall(id, name, args string) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id, Name: name, Arguments: args}}
}

func op(t *testing.T, id, call, name, args string) operation.Operation {
	t.Helper()
	data, _ := json.Marshal(toolreg.Plan{Call: call, Tool: name, Args: json.RawMessage(args)})
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: toolreg.PlanType, Version: toolreg.PlanVersion, Data: data})
	if err != nil {
		t.Fatal(err)
	}
	return operation.Operation{MaxOutputLength: spec.MaxOutputLength, ID: operation.ID(id), Type: spec.Type, Version: spec.Version, Status: operation.StatusReady, State: spec.State}
}

func finish(t *testing.T, o operation.Operation, status operation.Status, result, errText string, h toolreg.Handle) operation.Operation {
	t.Helper()
	s, err := operation.DecodeRemoteJobState(o)
	if err != nil {
		t.Fatal(err)
	}
	s.TerminalResult, s.TerminalError = result, errText
	s.Handle, _ = json.Marshal(h)
	st, err := operation.UpdateRemoteJob(o, s, status)
	if err != nil {
		t.Fatal(err)
	}
	return *st.Operation
}

func (b *builder) status(call string, st tool.CallStatus, ops ...operation.Operation) step {
	return b.item(sessionstore.ItemToolCallStatus, sessionstore.ToolCallStatus{TurnID: "t", CallID: call, Status: st, Operations: ops})
}

func waiting(ops ...operation.Operation) tool.CallStatus {
	var ids []operation.ID
	for _, o := range ops {
		ids = append(ids, o.ID)
	}
	return tool.CallStatus{WaitingFor: ids}
}

func control(t *testing.T, id string, m inbox.ControlMessage) inbox.Input {
	t.Helper()
	raw, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}
	return inbox.Input{ID: inbox.ID(id), Kind: inbox.InputControl, Payload: raw}
}

func delta(seq uint64, attempt int, kind agentllm.DeltaKind, text string) step {
	return step{delta: &agentllm.Delta{Seq: seq, Attempt: attempt, Kind: kind, Text: text, Name: text}}
}

func run(p *Projector, steps []step) []Out {
	var out []Out
	for _, s := range steps {
		switch {
		case s.item != nil:
			out = append(out, p.Item(*s.item)...)
		case s.meta != nil:
			p.Meta(*s.meta)
		case s.delta != nil:
			out = append(out, p.Delta(*s.delta)...)
		case s.adopt != nil:
			p.Adopt(s.adopt.call, s.adopt.job)
		case s.progress[0] != "":
			out = append(out, p.Progress(s.progress[0], s.progress[1])...)
		}
	}
	return out
}

func golden(t *testing.T, name string, outs []Out) {
	t.Helper()
	if outs == nil {
		outs = []Out{}
	}
	got, err := json.MarshalIndent(outs, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	got = append(got, '\n')
	path := filepath.Join("testdata", name+".json")
	if *update {
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run with -update to create it)", err)
	}
	if string(got) != string(want) {
		t.Errorf("%s: projection changed\n--- got\n%s\n--- want\n%s", name, got, want)
	}
}

// TestGoldens is one golden per P row of §10.1: what each harness item
// becomes for bough's readers.
func TestGoldens(t *testing.T) {
	t.Parallel()
	cases := map[string]func(t *testing.T) (Config, []step){
		// ItemInput: external and settings/stop are silent; a heartbeat is a quiet system row.
		"input_control": func(t *testing.T) (Config, []step) {
			var b builder
			return cfg(), []step{
				b.item(sessionstore.ItemInput, inbox.Input{ID: "in-1", Kind: inbox.InputExternal, Payload: json.RawMessage(`"hi"`)}),
				b.item(sessionstore.ItemInput, control(t, "c-1", inbox.ControlMessage{Mode: inbox.UpdateSettings, Parameters: inbox.Settings{ReasoningEffort: "high"}})),
				b.item(sessionstore.ItemInput, control(t, "c-2", inbox.ControlMessage{Mode: inbox.Heartbeat, Reason: "3 calls still running"})),
				b.item(sessionstore.ItemInput, control(t, "c-3", inbox.ControlMessage{Mode: inbox.StopWhenIdle})),
				b.turn("t1"),
			}
		},
		// Deltas: only the newest Seq streams; a new Seq or Attempt after shown text resets.
		"deltas": func(t *testing.T) (Config, []step) {
			return cfg(), []step{
				delta(1, 1, agentllm.DeltaStart, ""),
				delta(1, 1, agentllm.DeltaThinking, "plan"),
				delta(1, 1, agentllm.DeltaText, "Hel"),
				delta(1, 1, agentllm.DeltaRetry, ""),
				delta(1, 2, agentllm.DeltaText, "Hello"),
				delta(1, 2, agentllm.DeltaToolStart, "bash"),
				delta(2, 1, agentllm.DeltaText, "again"),
				delta(1, 2, agentllm.DeltaText, " stale"),
				delta(3, 1, agentllm.DeltaStart, ""),
			}
		},
		// A response: thinking, assistant with provenance, calls learned but not yet shown.
		"response": func(t *testing.T) (Config, []step) {
			var b builder
			return cfg(), []step{
				{meta: &Meta{ResponseID: "r1", Model: "claude-opus-5-5", Provider: "anthropic"}},
				b.response("r1", "", reasoning("Looking at the tree.", "Then the tests."), text("I'll run the tests."), toolCall("c1", "bash", `{"command":"go test ./..."}`)),
				{meta: &Meta{ResponseID: "r2", Model: "gpt-5.6-sol", Provider: "openai", Partial: true}},
				b.response("r2", "", text("Half a rep")),
				b.response("r3", "", text("no meta")),
			}
		},
		// Muted and errored responses project nothing; max_output_tokens says so.
		"response_muted_err_cut": func(t *testing.T) (Config, []step) {
			var b builder
			return cfg(), []step{
				{meta: &Meta{ResponseID: "m", Muted: true}},
				b.response("m", ""),
				{meta: &Meta{ResponseID: "e", Err: "HTTP 529"}},
				b.response("e", ""),
				{meta: &Meta{ResponseID: "x", Model: "m"}},
				b.response("x", ullm.StopMaxOutputTokens, text("and then the")),
			}
		},
		// A call's life: live start, live output, recorded end with its evidence.
		"call": func(t *testing.T) (Config, []step) {
			var b builder
			o := op(t, "op1", "c1", "bash", `{"command":"go test ./...\necho ok"}`)
			done := finish(t, o, operation.StatusCompleted, "ok  pkg\nFAIL other\n", "", toolreg.Handle{Data: map[string]any{"exit": 1, "cmd": "go test ./...\necho ok"}, MS: 999})
			steps := []step{
				b.response("r1", "", toolCall("c1", "bash", `{"command":"go test ./...\necho ok"}`)),
				b.status("c1", waiting(o), o),
				{progress: [2]string{"c1", "ok  pkg\n"}},
			}
			b.wait(1500)
			return cfg(), append(steps, b.status("c1", waiting(o), done))
		},
		// An edit call carries add/del; a failed call its error; a canceled one says so.
		"call_edit_fail_cancel": func(t *testing.T) (Config, []step) {
			var b builder
			w := op(t, "w", "cw", "write", `{"path":"a.go","content":"x"}`)
			f := op(t, "f", "cf", "view", `{"path":"nope.go"}`)
			k := op(t, "k", "ck", "bash", `{"command":"sleep 100"}`)
			return cfg(), []step{
				b.response("r1", "", toolCall("cw", "write", `{"path":"a.go","content":"x"}`), toolCall("cf", "view", `{"path":"nope.go"}`), toolCall("ck", "bash", `{"command":"sleep 100"}`)),
				b.status("cw", waiting(w), w),
				b.status("cf", waiting(f), f),
				b.status("ck", waiting(k), k),
				b.status("cw", waiting(w), finish(t, w, operation.StatusCompleted, "wrote a.go (+1 −0)", "", toolreg.Handle{Data: map[string]any{"add": 1, "del": 0, "path": "a.go"}})),
				b.status("cf", waiting(f), finish(t, f, operation.StatusFailed, "", "open nope.go: no such file", toolreg.Handle{})),
				b.status("ck", waiting(k), finish(t, k, operation.StatusCanceled, "", "", toolreg.Handle{Error: "cancelled by the user"})),
			}
		},
		// A status that failed at translate time is recorded at once, ms 0, no start row.
		"call_translate_error": func(t *testing.T) (Config, []step) {
			var b builder
			return cfg(), []step{
				b.response("r1", "", toolCall("c1", "nope", `{}`), toolCall("c2", "run_js", `{"code":"tools.nope()"}`)),
				b.status("c1", tool.CallStatus{Error: `tool "nope" is not available in this session`}),
				b.status("c2", tool.CallStatus{Error: "Error: arguments must be a JSON object"}),
			}
		},
		// A call running across an ItemTurn is late; an adopted call carries its job.
		"call_late_adopted": func(t *testing.T) (Config, []step) {
			var b builder
			o := op(t, "op1", "c1", "bash", `{"command":"make build"}`)
			return cfg(), []step{
				b.response("r1", "", toolCall("c1", "bash", `{"command":"make build"}`)),
				b.status("c1", waiting(o), o),
				b.turn("t2"),
				{adopt: &struct {
					call string
					job  int
				}{"c1", 3}},
				b.status("c1", waiting(o), finish(t, o, operation.StatusCompleted, "built", "", toolreg.Handle{Data: map[string]any{"exit": 0}})),
			}
		},
		// run_js projects as code/result, and a failure puts a live error first.
		"run_js": func(t *testing.T) (Config, []step) {
			var b builder
			a := op(t, "a", "j1", "run_js", `{"code":"print(1)"}`)
			e := op(t, "e", "j2", "run_js", `{"code":"throw 1"}`)
			return cfg(), []step{
				b.response("r1", "", toolCall("j1", "run_js", `{"code":"print(1)"}`), toolCall("j2", "run_js", `{"code":"throw 1"}`)),
				b.status("j1", waiting(a), a),
				b.status("j2", waiting(e), e),
				b.status("j1", waiting(a), finish(t, a, operation.StatusCompleted, "1\n", "", toolreg.Handle{})),
				b.status("j2", waiting(e), finish(t, e, operation.StatusFailed, "", "Uncaught 1", toolreg.Handle{})),
			}
		},
		// A child projects under sub:, with a numeric worker; its deltas, thinking and progress stay home.
		"child": func(t *testing.T) (Config, []step) {
			var b builder
			o := op(t, "op1", "c1", "view", `{"path":"README.md"}`)
			c := cfg()
			c.Prefix, c.Worker = "sub:", "2"
			return c, []step{
				delta(1, 1, agentllm.DeltaText, "streaming"),
				b.response("r1", "", reasoning("hmm"), text("Reading."), toolCall("c1", "view", `{"path":"README.md"}`)),
				b.status("c1", waiting(o), o),
				{progress: [2]string{"c1", "# bough"}},
				b.status("c1", waiting(o), finish(t, o, operation.StatusCompleted, "# bough\n", "", toolreg.Handle{})),
			}
		},
		// Catch-up that starts after the response: the plan names the tool.
		"catchup_from_plan": func(t *testing.T) (Config, []step) {
			var b builder
			o := op(t, "op1", "c9", "patch", `{"path":"x.go","old":"a","new":"b"}`)
			return cfg(), []step{
				b.status("c9", waiting(o), finish(t, o, operation.StatusCompleted, "patched x.go", "", toolreg.Handle{Data: map[string]any{"add": 1, "del": 1}, MS: 42})),
			}
		},
		// Output past row_output keeps head and tail and says it was cut.
		"call_truncated": func(t *testing.T) (Config, []step) {
			var b builder
			o := op(t, "op1", "c1", "bash", `{"command":"seq 1000"}`)
			c := cfg()
			c.RowOutput = 40
			var sb strings.Builder
			for i := range 30 {
				sb.WriteString(strings.Repeat("x", i%7) + "\n")
			}
			return c, []step{
				b.response("r1", "", toolCall("c1", "bash", `{"command":"seq 1000"}`)),
				b.status("c1", waiting(o), o),
				b.status("c1", waiting(o), finish(t, o, operation.StatusCompleted, sb.String(), "", toolreg.Handle{Data: map[string]any{"exit": 0}})),
			}
		},
	}
	for name, mk := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			c, steps := mk(t)
			golden(t, name, run(New(c), steps))
		})
	}
}

// Every recorded row carries the item's hseq and its own text, so the
// actor can append Data as it is and catch-up can resume past it.
func TestRecordedRowsCarryHseqAndText(t *testing.T) {
	t.Parallel()
	var b builder
	o := op(t, "op1", "c1", "bash", `{"command":"ls"}`)
	steps := []step{
		b.response("r1", ullm.StopMaxOutputTokens, reasoning("x"), text("y"), toolCall("c1", "bash", `{"command":"ls"}`)),
		b.status("c1", waiting(o), o),
		b.status("c1", waiting(o), finish(t, o, operation.StatusCompleted, "a\n", "", toolreg.Handle{})),
	}
	outs := run(New(cfg()), steps)
	recorded := 0
	for _, o := range outs {
		if !o.Record {
			if _, ok := o.Data["hseq"]; ok {
				t.Errorf("live %s carries hseq", o.Kind)
			}
			continue
		}
		recorded++
		if _, ok := o.Data["hseq"].(uint64); !ok {
			t.Errorf("recorded %s has no hseq: %v", o.Kind, o.Data)
		}
		if o.Data["text"] != o.Text {
			t.Errorf("recorded %s: data.text %q != Text %q", o.Kind, o.Data["text"], o.Text)
		}
	}
	if recorded != 4 { // thinking, assistant, system, call
		t.Errorf("recorded %d rows, want 4: %+v", recorded, outs)
	}
}

// A terminal status seen twice (a replay that overlaps) is one row.
func TestTerminalRowOnce(t *testing.T) {
	t.Parallel()
	var b builder
	o := op(t, "op1", "c1", "bash", `{"command":"ls"}`)
	d := finish(t, o, operation.StatusCompleted, "a\n", "", toolreg.Handle{})
	outs := run(New(cfg()), []step{b.status("c1", waiting(o), d), b.status("c1", waiting(o), d)})
	n := 0
	for _, o := range outs {
		if o.Kind == "call" && o.Record {
			n++
		}
	}
	if n != 1 {
		t.Fatalf("got %d call rows, want 1", n)
	}
	p := New(cfg())
	run(p, []step{b.response("r", "", toolCall("c2", "bash", `{"command":"echo hi"}`))})
	if tl, det, ok := p.Call("c2"); !ok || tl != "bash" || det != "echo hi" {
		t.Fatalf("Call = %q %q %v", tl, det, ok)
	}
}

// view_image is the harness op, not bough.call: its row never carries
// the image bytes.
func TestViewImageRowHasNoBytes(t *testing.T) {
	t.Parallel()
	spec, err := operation.NewViewImageSpec("/tmp/a.png", operation.ViewImageConfig{})
	if err != nil {
		t.Skipf("view_image spec: %v", err)
	}
	o := operation.Operation{MaxOutputLength: 1000, ID: "v", Type: spec.Type, Version: spec.Version, Status: operation.StatusCompleted}
	s, _ := operation.DecodeViewImageState(operation.Operation{Type: spec.Type, Version: spec.Version, State: spec.State})
	s.Result = &operation.ViewImageResult{Content: strings.Repeat("QUJD", 1000), OriginalWidth: 10, OriginalHeight: 20, OriginalMIMEType: "image/png"}
	o.State, _ = json.Marshal(s)
	var b builder
	outs := run(New(cfg()), []step{
		b.response("r", "", toolCall("v1", "view_image", `{"path":"/tmp/a.png"}`)),
		b.status("v1", waiting(o), o),
	})
	last := outs[len(outs)-1]
	if last.Kind != "call" || !last.Record {
		t.Fatalf("last out = %+v", last)
	}
	if out, _ := last.Data["output"].(string); strings.Contains(out, "QUJD") || !strings.Contains(out, "10×20 image/png") {
		t.Fatalf("output = %q", out)
	}
}

func TestHeadTail(t *testing.T) {
	t.Parallel()
	if got := headTail("short", 10); got != "short" {
		t.Fatal(got)
	}
	s := strings.Repeat("é", 50)
	got := headTail(s, 21)
	if len(got) > 21 || !strings.Contains(got, "…") {
		t.Fatalf("%q (%d)", got, len(got))
	}
	for _, r := range got {
		if r == '�' {
			t.Fatalf("cut inside a rune: %q", got)
		}
	}
}
