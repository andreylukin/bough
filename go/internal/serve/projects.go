package serve

// Projects: named groupings of conversations, defined by hand.
//
// A project is a LABEL, not a container. It owns no sessions, holds no
// settings, and deleting one never deletes a conversation — it just
// unassigns them. That keeps the grouping cheap to change, which is the
// point: a working set is something you re-cut often.

import (
	"fmt"
	"net/http"
)

func (a *API) listProjects(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"projects": a.sup.Projects()})
}

func (a *API) createProject(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name string `json:"name"`
	}
	if !decode(w, r, &body) {
		return
	}
	p, err := a.sup.NewProject(body.Name)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"project": p})
}

func (a *API) renameProject(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Name string `json:"name"`
	}
	if !decode(w, r, &body) {
		return
	}
	if err := a.sup.RenameProject(id, body.Name); err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) deleteProject(w http.ResponseWriter, r *http.Request) {
	if err := a.sup.DeleteProject(r.PathValue("id")); err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// assignProject moves one session into a grouping; an empty project id
// takes it out of whichever it was in.
func (a *API) assignProject(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Project string `json:"project"`
	}
	if !decode(w, r, &body) {
		return
	}
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	if err := a.sup.AssignProject(id, body.Project); err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
