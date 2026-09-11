package vtreal

// A tools.ask is pending when the user opens /model, swaps provider,
// opens the picker again and escapes it, then answers the ask by
// number. The ask must survive the picker (and the swap must not wait
// on it), the turn in flight finishes on the llm it started with (the
// loop's remount handover), and the next turn goes to the swapped-in
// provider (a fake OpenAI Responses server that records each request
// body) with the answer in its history; its assistant entry carries
// the NEW model and provider.
//
// The llm row carries both the replay tape and a base_url: replay
// ignores base_url, and the swap keeps the row's config, so llm-openai
// mounts pointed at the fake server.

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const askThenModelPickerSwapAnswerWord = "QUASAR77"

// askThenModelPickerSwapAnswerServer answers every call with one stop
// reply and records each request body.
func askThenModelPickerSwapAnswerServer(t *testing.T) (*httptest.Server, func() []string) {
	var mu sync.Mutex
	var bodies []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		b, _ := io.ReadAll(r.Body)
		mu.Lock()
		bodies = append(bodies, string(b))
		mu.Unlock()
		text := askThenModelPickerSwapAnswerWord + " noted.\n\n```stop\nDone.\n```"
		completed, _ := json.Marshal(map[string]any{
			"type": "response.completed",
			"response": map[string]any{
				"status": "completed",
				"output": []any{map[string]any{"type": "message",
					"content": []any{map[string]any{"type": "output_text", "text": text}}}},
				"usage": map[string]any{"input_tokens": 10, "output_tokens": 5},
			},
		})
		delta, _ := json.Marshal(map[string]any{"type": "response.output_text.delta", "delta": text})
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprintf(w, "data: %s\n\ndata: %s\n\ndata: [DONE]\n\n", delta, completed)
	}))
	t.Cleanup(srv.Close)
	return srv, func() []string {
		mu.Lock()
		defer mu.Unlock()
		return append([]string(nil), bodies...)
	}
}

func askThenModelPickerSwapAnswerConfig(tape, srv string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q, base_url: %q}
- id: ask
  plugin: ask
  config: {timeout_minutes: 1}
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`, tape, srv)
}

func TestAskThenModelPickerSwapAnswer(t *testing.T) {
	t.Parallel()
	t.Run("SwapWhileAskPending", func(t *testing.T) {
		t.Parallel()
		askThenModelPickerSwapAnswerRun(t)
	})
}

func askThenModelPickerSwapAnswerRun(t *testing.T) {
	srv, bodies := askThenModelPickerSwapAnswerServer(t)
	tape, err := filepath.Abs("testdata/replay/ask.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	// No models.json stub (costHome's empties the catalogue): the
	// embedded snapshot lists llm-openai's models in the picker.
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, ".bough", "history"), 0o755); err != nil {
		t.Fatal(err)
	}
	a := costStart(t, home, askThenModelPickerSwapAnswerConfig(tape, srv.URL))
	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor("? Pick a color")
	a.settled()

	// Swap to an llm-openai model through the picker.
	a.modelEffortOpenPicker()
	a.typeText("llm-openai")
	a.waitUntil(func(s string) bool {
		m := modelEffortCursor.FindStringSubmatch(s)
		return m != nil && strings.HasPrefix(m[1], "llm-openai ")
	}, "cursor on an llm-openai row")
	choice := modelEffortCursor.FindStringSubmatch(a.text())[1]
	prov, mdl, _ := strings.Cut(choice, " ")
	a.key(uv.KeyEnter, 0)
	a.waitFor("model: " + prov + " · " + mdl)

	// Open the picker again and leave it with esc.
	a.modelEffortOpenPicker()
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "picker to close")
	s := a.settled()
	if !strings.Contains(s, "? Pick a color") || strings.Contains(s, "Color locked in.") {
		t.Fatalf("the ask is no longer pending after the picker closed:\n%s", s)
	}
	for _, e := range pasteEntries(a) {
		if e.Kind == "ask/answer" {
			t.Fatalf("ask answered by the picker: %v", e.Data)
		}
	}
	if n := len(bodies()); n != 0 {
		t.Fatalf("the swap alone sent %d requests to the new provider", n)
	}
	a.check("picker closed, ask pending")

	// Answer by number: the turn finishes on the tape, then the next
	// turn is the new provider's.
	a.typeText("2")
	a.key(uv.KeyEnter, 0)
	a.waitFor("Color locked in.")
	a.settled()
	a.typeText("again")
	a.key(uv.KeyEnter, 0)
	a.waitFor(askThenModelPickerSwapAnswerWord)
	if got := askPasteAnswerEntry(a); got != "vermilion" {
		t.Errorf("ask/answer = %q, want vermilion", got)
	}
	deadline := time.Now().Add(15 * time.Second)
	for len(bodies()) == 0 && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
	}
	bs := bodies()
	if len(bs) == 0 {
		t.Fatalf("no request reached the swapped-in provider:\n%s", a.text())
	}
	if !strings.Contains(bs[0], "you picked vermilion") {
		t.Errorf("the next llm request lacks the answer's result:\n%.2000s", bs[0])
	}
	if !strings.Contains(bs[0], `"model":"`+mdl+`"`) {
		t.Errorf("the next llm request is not for %s:\n%.500s", mdl, bs[0])
	}

	// The new provider's assistant entry (the last one) names the new
	// model/provider.
	var after []map[string]any
	deadline = time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		after = after[:0]
		seen := false
		for _, e := range pasteEntries(a) {
			if e.Kind == "ask/answer" {
				seen = true
			}
			if seen && e.Kind == "assistant" {
				after = append(after, e.Data)
			}
		}
		if len(after) > 1 {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if len(after) < 2 {
		t.Fatalf("no new-provider assistant entry after the answer:\n%s", a.text())
	}
	last := after[len(after)-1]
	if got, _ := last["model"].(string); got != mdl {
		t.Errorf("assistant entry model = %q, want %q (%v)", got, mdl, last)
	}
	if got, _ := last["provider"].(string); !strings.Contains(got, "openai") {
		t.Errorf("assistant entry provider = %q, want the swapped-in openai (%v)", got, last)
	}
	a.check("after answer")
}
