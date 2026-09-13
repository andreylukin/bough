package llm

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestOpenaiBody(t *testing.T) {
	o := &openaiLLM{model: "gpt-5.6-sol", effort: "xhigh"}
	b := o.body("sys", []Message{{Role: "user", Content: "hi"}, {Role: "assistant", Content: "yo"}}, true, false)
	if b["model"] != "gpt-5.6-sol" || b["instructions"] != "sys" || b["store"] != false || b["stream"] != true {
		t.Fatalf("body = %v", b)
	}
	if r := b["reasoning"].(map[string]any); r["effort"] != "xhigh" {
		t.Fatalf("reasoning = %v", r)
	}
	in := b["input"].([]map[string]any)
	if len(in) != 2 || in[0]["role"] != "user" || in[1]["role"] != "assistant" || in[1]["content"] != "yo" {
		t.Fatalf("input = %v", in)
	}
	if _, has := (&openaiLLM{model: "m"}).body("", nil, false, false)["reasoning"]; has {
		t.Fatal("no effort should mean no reasoning field")
	}
}

func TestOpenaiReadStream(t *testing.T) {
	body := strings.Join([]string{
		`event: response.created`,
		`data: {"type":"response.created","response":{"status":"in_progress"}}`,
		``,
		`data: {"type":"response.output_text.delta","delta":"Hel"}`,
		`data: {"type":"response.output_text.delta","delta":"lo"}`,
		`data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning"},{"type":"message","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":7,"output_tokens":3}}}`,
	}, "\n")
	o := &openaiLLM{model: "m"}
	var got []string
	out, err := o.readStream(strings.NewReader(body), func(d string) { got = append(got, d) }, nil)
	if err != nil || out != "Hello" || strings.Join(got, "|") != "Hel|lo" {
		t.Fatalf("readStream = (%q, %v) deltas=%v", out, err, got)
	}
	if u := o.Usage(); u.InputTokens != 7 || u.OutputTokens != 3 || u.LastInputTokens != 7 || u.Priced {
		t.Fatalf("usage = %+v", u)
	}
	_, err = o.readStream(strings.NewReader(`data: {"type":"response.failed","response":{"status":"failed","error":{"message":"quota"}}}`), func(string) {}, nil)
	if err == nil || !strings.Contains(err.Error(), "quota") {
		t.Fatalf("failed event = %v", err)
	}
	_, err = o.readStream(strings.NewReader(`data: {"type":"response.output_text.delta","delta":"x"}`), func(string) {}, nil)
	if err == nil || !strings.Contains(err.Error(), "without response.completed") {
		t.Fatalf("cut stream = %v", err)
	}
}

func TestOpenaiErr(t *testing.T) {
	err := openaiErr(404, "nope", []byte(`{"error":{"message":"The model nope does not exist","type":"x","user_id":"u"}}`))
	if !strings.Contains(err.Error(), `model "nope" not found`) || strings.Contains(err.Error(), "user_id") {
		t.Fatalf("404 = %v", err)
	}
	if err := openaiErr(429, "m", []byte(`{"error":{"message":"rate limited"}}`)); !strings.Contains(err.Error(), "HTTP 429: rate limited") {
		t.Fatalf("429 = %v", err)
	}
}

// gpt-5.6 answers with a preamble message and then the answer, as two
// output items. Concatenated bare they ran together mid-sentence
// ("…open the PR.I need the target image tag…").
func TestOpenaiJoinsMessageItems(t *testing.T) {
	var r openaiResponse
	body := `{"status":"completed","output":[
	  {"type":"reasoning","content":[]},
	  {"type":"message","content":[{"type":"output_text","text":"I'm locating the service definition."}]},
	  {"type":"message","content":[{"type":"output_text","text":"I need the target image tag."}]}
	]}`
	if err := json.Unmarshal([]byte(body), &r); err != nil {
		t.Fatal(err)
	}
	got, err := (&openaiLLM{}).finish(&r)
	if err != nil {
		t.Fatal(err)
	}
	want := "I'm locating the service definition.\n\nI need the target image tag."
	if got != want {
		t.Fatalf("finish() = %q, want %q", got, want)
	}
}

// OpenAI caches prompts automatically; the response's
// input_tokens_details.cached_tokens must land in CacheReadTokens.
func TestOpenaiUsageCache(t *testing.T) {
	var r openaiResponse
	body := `{"status":"completed","output":[],"usage":{"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":90}}}`
	if err := json.Unmarshal([]byte(body), &r); err != nil {
		t.Fatal(err)
	}
	o := &openaiLLM{}
	if _, err := o.finish(&r); err != nil {
		t.Fatal(err)
	}
	u := o.Usage()
	if u.InputTokens != 100 || u.CacheReadTokens != 90 {
		t.Fatalf("usage = %+v, want InputTokens 100, CacheReadTokens 90", u)
	}
}

// Transcribe posts the WAV as multipart to /v1/audio/transcriptions
// and returns the text field.
func TestOpenaiTranscribe(t *testing.T) {
	var gotModel, gotName string
	var gotWav []byte
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/audio/transcriptions" || r.Header.Get("Authorization") != "Bearer k" {
			http.Error(w, r.URL.Path, 400)
			return
		}
		r.ParseMultipartForm(1 << 20)
		gotModel = r.FormValue("model")
		f, hdr, _ := r.FormFile("file")
		gotName = hdr.Filename
		gotWav, _ = io.ReadAll(f)
		w.Write([]byte(`{"text":"  fix the auth middleware \n"}`))
	}))
	defer srv.Close()
	o := &openaiLLM{model: "gpt-5", base: srv.URL, key: "k"}
	o.once.Do(func() {})
	text, err := o.Transcribe(context.Background(), []byte("RIFFwav"), "")
	if err != nil {
		t.Fatal(err)
	}
	if text != "fix the auth middleware" || gotModel != transcribeModel || gotName != "speech.wav" || string(gotWav) != "RIFFwav" {
		t.Fatalf("text=%q model=%q name=%q wav=%q", text, gotModel, gotName, gotWav)
	}
	var tr Transcriber = o
	_ = tr
}

// StreamThinking asks for a reasoning summary and decodes it into
// onThink, keeping it out of the reply.
func TestOpenaiStreamThinking(t *testing.T) {
	var reqBody map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewDecoder(r.Body).Decode(&reqBody)
		w.Write([]byte(strings.Join([]string{
			`data: {"type":"response.reasoning_summary_part.added"}`,
			`data: {"type":"response.reasoning_summary_text.delta","delta":"weighing "}`,
			`data: {"type":"response.reasoning_summary_text.delta","delta":"options"}`,
			`data: {"type":"response.reasoning_summary_part.added"}`,
			`data: {"type":"response.reasoning_summary_text.delta","delta":"decided"}`,
			`data: {"type":"response.output_text.delta","delta":"Hi"}`,
			`data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"Hi"}]}]}}`,
		}, "\n")))
	}))
	defer srv.Close()
	o := &openaiLLM{model: "gpt-6-astra", effort: "xhigh", base: srv.URL, key: "k"}
	o.once.Do(func() {})
	var ts ThinkingStreamer = o
	var reply, think strings.Builder
	out, err := ts.StreamThinking(context.Background(), "sys", []Message{{Role: "user", Content: "hi"}},
		func(d string) { reply.WriteString(d) }, func(d string) { think.WriteString(d) })
	if err != nil {
		t.Fatal(err)
	}
	if out != "Hi" || reply.String() != "Hi" {
		t.Fatalf("reply = %q / %q", out, reply.String())
	}
	if think.String() != "weighing options\n\ndecided" {
		t.Fatalf("thinking = %q", think.String())
	}
	r, _ := reqBody["reasoning"].(map[string]any)
	if r["effort"] != "xhigh" || r["summary"] != "auto" {
		t.Fatalf("reasoning = %v", r)
	}
}

// A model that returns no summary must say so in the thinking pane:
// silence is indistinguishable from a bug on the bough side.
func TestOpenaiNoReasoningSummaryIsLegible(t *testing.T) {
	body := `data: {"type":"response.output_text.delta","delta":"Hi"}
data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"Hi"}]}]}}`
	o := &openaiLLM{model: "gpt-6-astra", effort: "xhigh"}
	var think strings.Builder
	if _, err := o.readStream(strings.NewReader(body), func(string) {}, func(d string) { think.WriteString(d) }); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(think.String(), "no reasoning summary") || !strings.Contains(think.String(), "xhigh") {
		t.Fatalf("note = %q", think.String())
	}

	// With reasoning off there is nothing to explain.
	off := &openaiLLM{model: "gpt-6-astra"}
	think.Reset()
	off.readStream(strings.NewReader(body), func(string) {}, func(d string) { think.WriteString(d) })
	if think.Len() != 0 {
		t.Fatalf("unexpected note = %q", think.String())
	}
}
