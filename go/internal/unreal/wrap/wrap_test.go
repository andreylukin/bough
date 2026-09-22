package wrap_test

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"net/http"
	"strings"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/llm/responsesapi"

	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/wrap"
)

// stub is an adapter with a chosen provenance that records what it was
// sent and answers with fn.
type stub struct {
	prov string
	seen []ullm.Request
	fn   func(n int, r ullm.Request) (ullm.Response, error)
}

func (s *stub) Provider() string { return s.prov }
func (s *stub) Model() string    { return "m" }
func (s *stub) Close() error     { return nil }
func (s *stub) Respond(_ context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	s.seen = append(s.seen, r)
	if s.fn == nil {
		return ullm.Response{Stop: ullm.StopComplete}, nil
	}
	return s.fn(len(s.seen), r)
}

func reasoning(raw string) ullm.Item {
	return ullm.Item{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Raw: jsontext.Value(raw)}}
}

func user(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: s}}
}

func countReasoning(in []ullm.Item) int {
	n := 0
	for _, it := range in {
		if it.Type == ullm.ItemReasoning {
			n++
		}
	}
	return n
}

// Reasoning is stamped with who produced it, goes back verbatim to that
// provider, and never to another: anthropic -> openai -> anthropic keeps
// Anthropic's block for Anthropic and shows OpenAI none of it.
func TestEnvelopeRoundTripAndForeignDrop(t *testing.T) {
	t.Parallel()
	thinking := `{"type":"thinking","thinking":"hm","signature":"sig"}`
	encrypted := `{"type":"reasoning","encrypted_content":"gAAA"}`
	anth := &stub{prov: "anthropic", fn: func(n int, r ullm.Request) (ullm.Response, error) {
		if n == 1 {
			return ullm.Response{Output: []ullm.Item{reasoning(thinking), fake.Text("a")}}, nil
		}
		return ullm.Response{}, nil
	}}
	oai := &stub{prov: "openai", fn: func(int, ullm.Request) (ullm.Response, error) {
		out := reasoning(encrypted)
		out.ProviderID = "rs_1"
		return ullm.Response{Output: []ullm.Item{out}}, nil
	}}
	a, o := wrap.Envelope(anth), wrap.Envelope(oai)

	in := []ullm.Item{user("q")}
	r1, err := a.Respond(t.Context(), ullm.Request{Input: in}, ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	p, item, ok := wrap.Unwrap(r1.Output[0].Data.(ullm.Reasoning).Raw)
	if !ok || p != "anthropic" || string(item) != thinking {
		t.Fatalf("stored reasoning = %s/%s, want the anthropic envelope around the verbatim block", p, item)
	}
	in = append(in, r1.Output...)
	in = append(in, user("switch"))

	r2, err := o.Respond(t.Context(), ullm.Request{Input: in}, ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if n := countReasoning(oai.seen[0].Input); n != 0 {
		t.Errorf("openai was sent %d foreign reasoning items", n)
	}
	if r2.Output[0].ProviderID != "openai|rs_1" {
		t.Errorf("ProviderID = %q, want it stamped", r2.Output[0].ProviderID)
	}
	in = append(in, r2.Output...)
	in = append(in, user("back"))

	if _, err := a.Respond(t.Context(), ullm.Request{Input: in}, ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	sent := anth.seen[1].Input
	if n := countReasoning(sent); n != 1 {
		t.Fatalf("anthropic should get its own block back and nothing else, got %d reasoning items", n)
	}
	for _, it := range sent {
		if r, ok := it.Data.(ullm.Reasoning); ok && string(r.Raw) != thinking {
			t.Errorf("anthropic got %s, want its verbatim block", r.Raw)
		}
		if it.ProviderID != "" {
			t.Errorf("a foreign ProviderID %q reached anthropic", it.ProviderID)
		}
	}
	// A raw that was never enveloped (a store from before the envelope)
	// is dropped rather than guessed at.
	anth.seen = nil
	a.Respond(t.Context(), ullm.Request{Input: []ullm.Item{user("x"), reasoning(thinking), user("y")}}, ullm.RequestOptions{})
	if n := countReasoning(anth.seen[0].Input); n != 0 {
		t.Errorf("an un-enveloped raw was sent")
	}
}

func TestLateMarksByPosition(t *testing.T) {
	t.Parallel()
	res := func(id string) ullm.Item {
		return ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: id, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: id}}}}
	}
	in := []ullm.Item{
		user("go"),
		fake.Call("a", "bash", "{}"), fake.Call("b", "bash", "{}"),
		res("a"), res("b"), // first results, right after their calls: native
		fake.Text("waiting"),
		res("b"),      // second result for b: late
		res("orphan"), // no call anywhere: late
		fake.Call("c", "bash", "{}"),
		user("steer"),
		res("c"), // first, in the run after its call even behind a user message: native
	}
	got := wrap.Late(in)
	want := []bool{false, false, false, false, false, false, true, true, false, false, false}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Errorf("Late = %v\n want %v", got, want)
	}
}

func TestLateResultsAsText(t *testing.T) {
	t.Parallel()
	inner := &stub{prov: "openrouter:anthropic"}
	in := []ullm.Item{
		user("go"),
		fake.Call("a", "view_image", "{}"),
		{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "a", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "running"}}}},
		fake.Text("waiting"),
		{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "a", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "a.png"}, {Kind: ullm.ToolResultImage, Value: "data:image/png;base64,AA=="}}}},
	}
	if _, err := wrap.LateResultsAsText(inner).Respond(t.Context(), ullm.Request{Input: in}, ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	sent := inner.seen[0].Input
	if _, ok := sent[2].Data.(ullm.ToolResult); !ok {
		t.Errorf("the first result stays native: %+v", sent[2])
	}
	m, ok := sent[4].Data.(ullm.Message)
	want := "<tool_result call_id=\"a\" name=\"view_image\">\na.png\n[image omitted: call view_image again]\n</tool_result>"
	if !ok || m.Role != ullm.RoleUser || m.Text != want {
		t.Errorf("late result = %+v\nwant user text %q", sent[4], want)
	}
	if _, ok := in[4].Data.(ullm.ToolResult); !ok {
		t.Error("the caller's slice was modified")
	}
}

func reasoning400(msg string) error {
	return fmt.Errorf("create response: %w", &responsesapi.APIError{StatusCode: http.StatusBadRequest, Message: msg})
}

// A 400 about replayed reasoning strips it and retries once, and every
// later request goes without it; any other 400 is returned as is.
func TestStripReasoningOn400(t *testing.T) {
	t.Parallel()
	inner := &stub{prov: "openai", fn: func(n int, r ullm.Request) (ullm.Response, error) {
		if countReasoning(r.Input) > 0 {
			return ullm.Response{}, reasoning400("Encrypted content could not be decrypted or parsed.")
		}
		return ullm.Response{Stop: ullm.StopComplete}, nil
	}}
	a := wrap.StripReasoningOn400(inner)
	req := ullm.Request{Input: []ullm.Item{user("q"), reasoning(`{"type":"reasoning"}`), fake.Text("a"), user("r")}}
	if _, err := a.Respond(t.Context(), req, ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	if _, err := a.Respond(t.Context(), req, ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	if len(inner.seen) != 3 || countReasoning(inner.seen[2].Input) != 0 {
		t.Errorf("want one failed try, one stripped retry, then stripped up front; saw %d requests", len(inner.seen))
	}

	other := &stub{prov: "openai", fn: func(int, ullm.Request) (ullm.Response, error) {
		return ullm.Response{}, reasoning400("Invalid value for 'model'.")
	}}
	if _, err := wrap.StripReasoningOn400(other).Respond(t.Context(), req, ullm.RequestOptions{}); err == nil || len(other.seen) != 1 {
		t.Errorf("an unrelated 400 is not retried: err %v, %d requests", err, len(other.seen))
	}
}

func TestObserveSeesSuccessOnly(t *testing.T) {
	t.Parallel()
	var got []string
	ok := wrap.Observe(&stub{prov: "x", fn: func(int, ullm.Request) (ullm.Response, error) {
		return ullm.Response{ID: "r1"}, nil
	}}, func(r ullm.Response) { got = append(got, r.ID) })
	bad := wrap.Observe(&stub{prov: "x", fn: func(int, ullm.Request) (ullm.Response, error) {
		return ullm.Response{}, fmt.Errorf("boom")
	}}, func(r ullm.Response) { got = append(got, "bad") })
	ok.Respond(t.Context(), ullm.Request{}, ullm.RequestOptions{})
	bad.Respond(t.Context(), ullm.Request{}, ullm.RequestOptions{})
	if strings.Join(got, ",") != "r1" {
		t.Errorf("observed %v", got)
	}
	if ok.Provider() != "x" || ok.Model() != "m" {
		t.Error("decorators forward the identity")
	}
}
