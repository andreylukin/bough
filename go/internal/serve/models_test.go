package serve

import (
	"net/http"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func seedOne(t *testing.T, f *apiFixture, id string) {
	t.Helper()
	now := time.Now()
	f.seed(t, id,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/w"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hello"}},
		history.Entry{Seq: 3, At: now, Kind: "done", Data: nil},
	)
}

// The picker needs providers and the reasoning levels; without them it
// can only offer a text box.
func TestModelCatalogue(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	code, body := f.do(t, "GET", "/api/models", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/models = %d", code)
	}
	efforts, _ := body["efforts"].([]any)
	if len(efforts) == 0 {
		t.Error("no reasoning levels offered")
	}
	provs, _ := body["providers"].([]any)
	if len(provs) == 0 {
		t.Fatal("no providers offered")
	}
	// At least one provider must carry a real model list, or the picker
	// is empty for every session.
	withModels := 0
	for _, p := range provs {
		pm, _ := p.(map[string]any)
		if ms, _ := pm["models"].([]any); len(ms) > 0 {
			withModels++
		}
	}
	if withModels == 0 {
		t.Errorf("no provider carried a model list: %v", provs)
	}
}

// An unknown level must be refused BEFORE anything is written to a
// child: a typo should not reach the session as a bare prompt.
func TestSetEffortRejectsUnknownLevel(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedOne(t, f, "s1")
	code, body := f.do(t, "POST", "/api/sessions/s1/effort", `{"effort":"ludicrous"}`)
	if code == http.StatusOK {
		t.Fatalf("an unknown level was accepted: %v", body)
	}
	if msg, _ := body["error"].(string); msg == "" {
		t.Errorf("no error text explaining the refusal: %v", body)
	}
}

func TestSetModelRequiresAModel(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedOne(t, f, "s1")
	if code, body := f.do(t, "POST", "/api/sessions/s1/model", `{"model":"  "}`); code == http.StatusOK {
		t.Fatalf("an empty model was accepted: %v", body)
	}
}

func TestModelAndEffortOnUnknownSession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	for _, path := range []string{"/api/sessions/nope/model", "/api/sessions/nope/effort"} {
		if code, _ := f.do(t, "POST", path, `{"model":"x","effort":"high"}`); code != http.StatusNotFound {
			t.Errorf("POST %s on an unknown session = %d, want 404", path, code)
		}
	}
}

// A session that was never told what to run reports nothing, rather
// than inventing a model it is not using.
func TestRowOmitsModelUntilSet(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedOne(t, f, "s1")
	_, body := f.do(t, "GET", "/api/sessions/s1", "")
	sess, _ := body["session"].(map[string]any)
	if _, ok := sess["model"]; ok {
		t.Errorf("model reported before it was ever set: %v", sess)
	}
	if _, ok := sess["effort"]; ok {
		t.Errorf("effort reported before it was ever set: %v", sess)
	}
}
