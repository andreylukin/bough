package serve

// The model catalogue, and the two controls that change how a session
// thinks. Both write the command a person would type (/model, /think)
// to the child's stdin, so the web UI and the TUI drive the same code
// path — there is no second way to change a model to keep in step.

import (
	"net/http"
	"strings"

	"github.com/andreylukin/bough/internal/models"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// catalogueCap is how many models one provider contributes. The
// catalogue carries hundreds for some providers, and a list that long
// is not a choice; newest first, so the cap keeps what someone is
// likely reaching for. Any id still works when typed.
const catalogueCap = 14

// ModelInfo is one model as the picker shows it.
type ModelInfo struct {
	ID      string   `json:"id"`
	Context int      `json:"context,omitempty"` // tokens
	Efforts []string `json:"efforts,omitempty"` // reasoning levels it accepts
	Input   float64  `json:"input,omitempty"`   // $ per Mtok
	Output  float64  `json:"output,omitempty"`
	Release string   `json:"release,omitempty"`
}

// ProviderInfo groups a provider's models.
// ModelDefault is what a session runs as when nobody picked: the llm
// row of the config serve started with. The picker names it instead of
// saying "Default", which named nothing.
type ModelDefault struct {
	Plugin string `json:"plugin"`
	Model  string `json:"model"`
	Effort string `json:"effort,omitempty"` // "" = the provider's own default
}

type ProviderInfo struct {
	Plugin string      `json:"plugin"`
	Models []ModelInfo `json:"models"`
}

// models lists what a session can be switched to, per provider.
func (a *API) models(w http.ResponseWriter, r *http.Request) {
	var out []ProviderInfo
	for _, name := range kernel.Plugins() {
		if !strings.HasPrefix(name, "llm-") {
			continue
		}
		ids := models.List(name, catalogueCap)
		if len(ids) == 0 {
			// A provider with no curated list is still selectable by
			// name; the UI can offer it with a free-text model.
			out = append(out, ProviderInfo{Plugin: name})
			continue
		}
		ms := make([]ModelInfo, 0, len(ids))
		for _, id := range ids {
			mi := ModelInfo{ID: id}
			if m, ok := models.Lookup(name, id); ok {
				mi.Context, mi.Efforts = m.Context, m.Efforts
				mi.Input, mi.Output, mi.Release = m.Input, m.Output, m.Release
			}
			ms = append(ms, mi)
		}
		out = append(out, ProviderInfo{Plugin: name, Models: ms})
	}
	// max is offered beside the shift+tab cycle, not in it: the rows
	// accept it (clamped per model), and a model with its own list says
	// whether it has it.
	resp := map[string]any{"providers": out, "efforts": llm.Levels()}
	if a.defaults != nil {
		if d := a.defaults(a.start); d.Model != "" {
			resp["default"] = d
		}
	}
	writeJSON(w, http.StatusOK, resp)
}

// SetDefaults wires in how a child started in dir resolves its llm row:
// /api/models names serve's own (dir = where serve started), and each
// local session's row names its own, since a repo's bough.yml can run
// something else. A func, read per request: the launcher re-reads
// bough.yml, so an edit shows on the next load without restarting serve.
func (a *API) SetDefaults(f func(dir string) ModelDefault) { a.defaults = f }

func (a *API) setModel(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Model  string `json:"model"`
		Plugin string `json:"plugin"` // the provider that owns the model
	}
	if !decode(w, r, &body) {
		return
	}
	// race is a model test's tag for BOUGH_TEST_STEP_GATE (see
	// Supervisor.pick); no production caller sends it.
	race := r.URL.Query().Get("race")
	a.metaVerb(w, id, func() error { return a.sup.SetModel(id, body.Plugin, body.Model, race) })
}

func (a *API) setEffort(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Effort string `json:"effort"`
	}
	if !decode(w, r, &body) {
		return
	}
	a.metaVerb(w, id, func() error { return a.sup.SetEffort(id, body.Effort) })
}
