package serve

// The skill catalogue. A skill runs by being the first thing in a
// message ("/grill-me the design"), which the headless child already
// dispatches before it steers or submits — so the web picker only
// has to name what exists and let someone put it in the composer.

import (
	"net/http"

	"github.com/andreylukin/bough/plugins/skills"
)

// skills lists every skill the agent would find, so the composer can
// offer the same set the TUI's slash palette does.
func (a *API) skills(w http.ResponseWriter, r *http.Request) {
	cat := skills.Default(a.home).Catalog()
	if cat == nil {
		cat = []skills.SkillInfo{} // an empty list, never a JSON null
	}
	writeJSON(w, http.StatusOK, map[string]any{"skills": cat})
}
