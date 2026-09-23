package serve

import (
	"errors"
	"fmt"
	"net/http"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// createChild is POST /api/sessions with spawnedBy: a session starting a
// background agent. The row comes back at once, queued or not; a child
// that has not written its history yet answers with what serve knows.
func (a *API) createChild(w http.ResponseWriter, opt CreateOptions, maxPerSession, maxRunning int) {
	if opt.Slug != "" {
		if projectdef.ValidSlug(opt.Slug) != nil {
			writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown project %q", opt.Slug))
			return
		}
		if _, err := projectdef.Load(a.sup.Home(), opt.Slug); err != nil {
			writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown project %q", opt.Slug))
			return
		}
	}
	id, queued, err := a.sup.CreateChild(opt, maxPerSession, maxRunning)
	switch {
	case errors.Is(err, ErrUnknownSession):
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", opt.SpawnedBy))
		return
	case errors.Is(err, ErrDepth):
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: session %q is itself a background agent (depth 1)", opt.SpawnedBy))
		return
	case errors.Is(err, ErrAgentLimit):
		n := maxPerSession
		if n <= 0 {
			n = defaultMaxPerSession
		}
		writeErr(w, http.StatusTooManyRequests, fmt.Errorf("serve: api: background agent limit reached (%d per session)", n))
		return
	case err != nil && id == "":
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: create background agent: %w", err))
		return
	}
	row := a.queuedRow(id)
	if !queued {
		row.Queued = false
		row.Status = StatusRunning
		row.Live = a.sup.Live(id)
		if in, ok := a.info(id); ok {
			row = a.row(in)
		}
	}
	writeJSON(w, http.StatusCreated, map[string]any{"session": row, "queued": queued})
}

// queuedRow is the row of a child with no history file yet.
func (a *API) queuedRow(id string) Row {
	m := a.sup.Meta(id)
	prompt, _ := a.sup.QueuedPrompt(id)
	// A queued child has no history yet; only a project spawn carries a
	// project, so the membership is what says where it will run.
	mode := "local"
	if m.Project != "" {
		mode = "project"
	}
	now := time.Now()
	return Row{
		ID:        id,
		Title:     oneLineTitle(prompt),
		Status:    StatusQueued,
		Queued:    true,
		SpawnedBy: m.SpawnedBy,
		Project:   m.Project,
		Mode:      mode,
		Modified:  now,
		LastAt:    now,
	}
}

// pendingRows are the sessions history.List cannot see yet — queued
// children, and live ones still booting — skipping ids in seen. Both
// /api/sessions and a project's page add exactly these, so the sidebar
// and the page never disagree about a thread that has not written yet.
func (a *API) pendingRows(seen map[string]bool) []Row {
	var rows []Row
	for _, id := range a.sup.queuedIDs() {
		if !seen[id] {
			rows = append(rows, a.queuedRow(id))
		}
	}
	for _, id := range a.sup.startingIDs() {
		if seen[id] || a.sup.Meta(id).Archived {
			continue
		}
		row := a.queuedRow(id)
		row.Queued, row.Status, row.Live = false, StatusRunning, true
		rows = append(rows, row)
	}
	return rows
}

func (a *API) agentCount(id string) *AgentCount {
	running, queued, total := a.sup.agentCounts(id)
	if total == 0 {
		return nil
	}
	return &AgentCount{Running: running, Queued: queued, Total: total}
}

// children is GET /api/sessions/{id}/children: every child as a row,
// queued ones included.
func (a *API) children(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if _, ok := a.info(id); !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	rows := []Row{}
	for _, c := range a.sup.Children(id) {
		if c.Queued {
			rows = append(rows, a.queuedRow(c.ID))
			continue
		}
		if in, ok := a.info(c.ID); ok {
			row := a.row(in)
			row.Error = c.Error
			rows = append(rows, row)
			continue
		}
		// Started but its history file is not written yet: still a child.
		row := a.queuedRow(c.ID)
		row.Queued, row.Status, row.Live = false, StatusRunning, a.sup.Live(c.ID)
		rows = append(rows, row)
	}
	writeJSON(w, http.StatusOK, map[string]any{"children": rows})
}

// agent is GET /api/sessions/{id}/agent?parent=: what tools.agent
// returns. Only the parent that started a child may read it.
func (a *API) agent(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	m, ok := a.childOf(w, id, r.URL.Query().Get("parent"))
	if !ok {
		return
	}
	out := map[string]any{"status": StatusQueued, "title": "", "reply": "", "project": "", "spawnedBy": m.SpawnedBy}
	if prompt, queued := a.sup.QueuedPrompt(id); queued {
		out["title"] = oneLineTitle(prompt)
		writeJSON(w, http.StatusOK, out)
		return
	}
	entries, _ := a.sup.Entries(id)
	st, _ := StatusOf(entries, a.sup.Live(id))
	_, slug := sessionMode(entries)
	out["status"] = st
	out["title"] = a.sup.childTitle(id)
	out["reply"] = lastTurn(entries).reply
	out["project"] = slug
	writeJSON(w, http.StatusOK, out)
}

// childOf checks a background agent exists and, when parent is given,
// belongs to it. It writes the error reply itself.
func (a *API) childOf(w http.ResponseWriter, id, parent string) (SessionMeta, bool) {
	m := a.sup.Meta(id)
	_, queued := a.sup.QueuedPrompt(id)
	// A child that started but has not written its file yet still exists.
	if _, onDisk := a.info(id); !onDisk && !queued && m.SpawnedBy == "" {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return m, false
	}
	if parent != "" && m.SpawnedBy != parent {
		writeErr(w, http.StatusForbidden, fmt.Errorf("serve: api: session %q was not started by %q", id, parent))
		return m, false
	}
	return m, true
}

// stopAgent is POST /api/sessions/{id}/stop.
func (a *API) stopAgent(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var body struct {
		Parent string `json:"parent"`
	}
	if r.ContentLength != 0 && !decode(w, r, &body) {
		return
	}
	if _, ok := a.childOf(w, id, body.Parent); !ok {
		return
	}
	was, err := a.sup.stopChild(id)
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: stop %q: %w", id, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "was": was})
}

// notify is POST /api/sessions/{id}/notify: a debug and acceptance
// handle on Supervisor.Notify.
func (a *API) notify(w http.ResponseWriter, r *http.Request) {
	a.textVerb(w, r, a.sup.Notify)
}
