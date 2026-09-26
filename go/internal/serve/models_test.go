package serve

import (
	"net/http"
	"reflect"
	"slices"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/models"
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
	// max is not in the shift+tab cycle (llm.Efforts), but the picker offers it.
	if len(efforts) > 0 && efforts[len(efforts)-1] != "max" {
		t.Errorf("efforts = %v, want max last", efforts)
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
		ms, _ := pm["models"].([]any)
		if len(ms) > 0 {
			withModels++
		}
		// Every model the catalogue has, not the newest few: a cap of
		// fourteen hid the model OpenRouter's config runs.
		plugin, _ := pm["plugin"].(string)
		if want := len(models.List(plugin, 0)); len(ms) != want {
			t.Errorf("%s offers %d models, want all %d", plugin, len(ms), want)
		}
	}
	if withModels == 0 {
		t.Errorf("no provider carried a model list: %v", provs)
	}
}

// The picker names the configured model and effort instead of
// "Default": a session that has not answered yet still runs as
// something, and the person should see what.
func TestModelCatalogueNamesTheDefault(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.SetDefaults(func(string) ModelDefault {
		return ModelDefault{Plugin: "llm-openrouter", Model: "openai/gpt-6-astra", Effort: "medium"}
	})
	_, body := f.do(t, "GET", "/api/models", "")
	d, _ := body["default"].(map[string]any)
	if d["plugin"] != "llm-openrouter" || d["model"] != "openai/gpt-6-astra" || d["effort"] != "medium" {
		t.Errorf("default = %v", body["default"])
	}
	// No configured model known: the key is absent, never an empty name.
	g := newAPI(t)
	g.api.SetDefaults(func(string) ModelDefault { return ModelDefault{} })
	_, body = g.do(t, "GET", "/api/models", "")
	if _, ok := body["default"]; ok {
		t.Errorf("an unknown default was reported: %v", body["default"])
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

// The picker's Configured option names the session's OWN llm row, not
// serve's: serve started from ~ runs sessions in repos with their own
// bough.yml. It stays after the session has answered (a row whose
// config names no model is named by what it answered as), and it is
// gone once anything switched the model, since nothing goes back.
func TestRowNamesTheSessionsConfiguredModel(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.SetDefaults(func(dir string) ModelDefault {
		if dir == "/repo" {
			return ModelDefault{Plugin: "llm-control", Effort: "low"}
		}
		return ModelDefault{Plugin: "llm-echo", Model: "serve-default"}
	})
	now := time.Now()
	meta := history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/repo", "mode": "local"}}
	turn := []history.Entry{
		meta,
		{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hi"}},
		{Seq: 3, At: now, Kind: "engine", Data: map[string]any{"model": "control", "provider": "control"}},
		{Seq: 4, At: now, Kind: "assistant", Data: map[string]any{"model": "control", "text": "hi"}},
		{Seq: 5, At: now, Kind: "done"},
	}
	f.seed(t, "fresh", meta)
	f.seed(t, "answered", turn...)
	f.seed(t, "switched", append(slices.Clone(turn),
		history.Entry{Seq: 6, At: now, Kind: "command", Data: map[string]any{"text": "/model llm-openai gpt-5.6"}},
		history.Entry{Seq: 7, At: now, Kind: "model", Data: map[string]any{"sets": []any{"llm.plugin=llm-openai", "llm.model=gpt-5.6"}}},
		history.Entry{Seq: 8, At: now, Kind: "system", Data: map[string]any{"text": "model: llm-openai · gpt-5.6"}},
	)...)
	f.seed(t, "picked", meta)
	f.sup.mu.Lock()
	f.sup.meta["picked"] = SessionMeta{Model: "gpt-5.6"}
	f.sup.mu.Unlock()

	want := map[string]any{
		"fresh":    map[string]any{"plugin": "llm-control", "model": "", "effort": "low"},
		"answered": map[string]any{"plugin": "llm-control", "model": "control", "effort": "low"},
		"switched": nil,
		"picked":   nil,
	}
	for id, w := range want {
		_, body := f.do(t, "GET", "/api/sessions/"+id, "")
		sess, _ := body["session"].(map[string]any)
		if got := sess["configured"]; !reflect.DeepEqual(got, w) {
			t.Errorf("%s: configured = %v, want %v", id, got, w)
		}
	}
	// /api/models still names serve's own row: that is serve's default.
	_, body := f.do(t, "GET", "/api/models", "")
	if d, _ := body["default"].(map[string]any); d["model"] != "serve-default" {
		t.Errorf("/api/models default = %v, want serve's own", body["default"])
	}
}
