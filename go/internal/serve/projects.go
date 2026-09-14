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
	"strings"
)

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

// byRepo lists the unassigned sessions grouped by the repo they worked
// in, so 150 conversations can be filed in a few clicks instead of one
// dropdown at a time.
func (a *API) byRepo(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"groups": a.repoGroups()})
}

// projectFromRepo makes one project out of one or more repos. Several
// on purpose: a project here is an area of work, not a repo. A person
// with hundreds of repos has a handful of areas, and "acme-api-py"
// is the name of a checkout, not the name of a thing you
// are doing. The caller names it.
func (a *API) projectFromRepo(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Repos []string `json:"repos"`
		Name  string   `json:"name"`
	}
	if !decode(w, r, &body) {
		return
	}
	want := map[string]bool{}
	for _, repo := range body.Repos {
		if repo = strings.TrimSpace(repo); repo != "" {
			want[repo] = true
		}
	}
	if len(want) == 0 {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: at least one repo is required"))
		return
	}
	name := strings.TrimSpace(body.Name)
	if name == "" {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: name is required"))
		return
	}

	var ids []string
	found := map[string]bool{}
	for _, g := range a.repoGroups() {
		if want[g.Repo] {
			found[g.Repo] = true
			ids = append(ids, g.Sessions...)
		}
	}
	for repo := range want {
		if !found[repo] {
			writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: no unassigned sessions for %q", repo))
			return
		}
	}

	p, err := a.sup.NewProject(name)
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	// A session that vanished between listing and assigning is not a
	// reason to fail the whole group; report how many actually moved.
	moved := 0
	for _, id := range ids {
		if err := a.sup.AssignProject(id, p.ID); err == nil {
			moved++
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"project": p, "moved": moved})
}
