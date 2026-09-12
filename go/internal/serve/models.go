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
	writeJSON(w, http.StatusOK, map[string]any{"providers": out, "efforts": llm.Efforts})
}

func (a *API) setModel(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Model string `json:"model"`
	}
	if !decode(w, r, &body) {
		return
	}
	a.metaVerb(w, id, func() error { return a.sup.SetModel(id, body.Model) })
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
