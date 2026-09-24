package serve

// The skill catalogue. A skill runs by being the first thing in a
// message ("/grill-me the design"), which the headless child already
// dispatches before it steers or submits — so the web picker only
// has to name what exists and let someone put it in the composer.

import (
	"fmt"
	"net/http"

	"github.com/andreylukin/bough/plugins/skills"
)

// skills lists every skill the agent would find, so the composer can
// offer the same set the TUI's slash palette does. With ?session=<id>
// it is that session's set: its cwd's .claude/skills, not serve's
// (serve runs from HOME, the session in its repo). Skills switched off
// are left out: the child never registers them as commands, so a pick
// of one would send an "unknown command". The session's context view
// still lists them, for the switch.
func (a *API) skills(w http.ResponseWriter, r *http.Request) {
	sk := skills.Default(a.home)
	if id := r.URL.Query().Get("session"); id != "" {
		in, ok := a.info(id)
		if !ok {
			writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
			return
		}
		sk = skills.DefaultFor(a.home, in.Cwd)
	}
	cat := []skills.SkillInfo{} // an empty list, never a JSON null
	for _, s := range sk.Catalog() {
		if !s.Off {
			cat = append(cat, s)
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"skills": cat})
}
