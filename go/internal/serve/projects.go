package serve

// Projects: the directories under ~/.bough/projects, as the web edits
// them.
//
// A project IS its directory — project.yml, the build scripts and
// MEMORY.md — so there is no table to keep in step and no id but the
// slug. Creating one makes the directory; renaming writes `name:` into
// project.yml; deleting removes the directory and the slug-keyed state
// around it, but never a conversation.

import (
	"errors"
	"fmt"
	"net/http"
	"sort"
	"strings"
)

// ProjectDetail is one project as its page reads it: the definition,
// the main thread, and every other session in the project.
type ProjectDetail struct {
	Project
	// Main is the main thread's session id, "" until the project has
	// been opened or messaged. Reading the page never creates one: a
	// glance at a project must not start a container.
	Main string `json:"main,omitempty"`
	// MainOrb is the main thread's container as the orb strip shows it,
	// nil when it has never started one.
	MainOrb *OrbState `json:"mainOrb,omitempty"`
	// Orbs is every session orb recorded for this project, newest first
	// — one per session, main's included.
	Orbs []OrbState `json:"orbs"`
	// Threads is every session in the project but main, oldest first.
	Threads []Row `json:"threads"`
}

// projectDetail is GET /api/projects/{slug}.
//
// Threads are the UNION of main's children and everything else filed
// under the project: a session started with `bough --project <slug>`,
// or one from before the project had a main thread, has no parent and
// would otherwise be missing from its own project's page. Those send no
// finish notices (see docs/orbs.md); they are still the person's work.
func (a *API) projectDetail(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	d := ProjectDetail{Project: p, Main: a.sup.MainID(p.Slug), Orbs: a.orbsOf(p.Slug), Threads: []Row{}}
	if d.Main != "" {
		if st := a.orbState(d.Main); st.Session != "" {
			d.MainOrb = &st
		}
	}
	seen := map[string]bool{d.Main: true}
	infos, err := a.sup.List()
	if err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: list sessions: %w", err))
		return
	}
	for _, in := range infos {
		if seen[in.ID] {
			continue
		}
		row := a.row(in)
		// row.Project is the slug the session's history recorded for a
		// project session, and the one a person filed it under for a
		// local one — both are membership in this project.
		if row.Project != p.Slug {
			continue
		}
		// Archived threads are folded away, as they are everywhere else
		// (listSessions hides them too): archiving one from its own
		// conversation, or archiving the whole project, has to be
		// visible on the surface it was invoked from.
		if row.Archived {
			seen[in.ID] = true
			continue
		}
		seen[in.ID] = true
		d.Threads = append(d.Threads, row)
	}
	// Children main started that have no history file yet (queued, or
	// still booting) are threads too, and the page has to show them or
	// a spawn looks like it did nothing.
	if d.Main != "" {
		for _, c := range a.sup.Children(d.Main) {
			if seen[c.ID] || a.sup.Meta(c.ID).Archived {
				continue
			}
			seen[c.ID] = true
			row := a.queuedRow(c.ID)
			if !c.Queued {
				row.Queued, row.Status, row.Live = false, StatusRunning, a.sup.Live(c.ID)
			}
			d.Threads = append(d.Threads, row)
		}
	}
	// Ids are time-ordered, so this is oldest first.
	sort.Slice(d.Threads, func(i, j int) bool { return d.Threads[i].ID < d.Threads[j].ID })
	writeJSON(w, http.StatusOK, d)
}

// messageProject sends to the project's main thread, creating it on the
// first message. Messaging the project IS messaging main: the project
// page shows main's conversation, and a person typing there is talking
// to the session that hands work to the threads.
func (a *API) messageProject(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	var body struct {
		Text string `json:"text"`
	}
	if !decode(w, r, &body) {
		return
	}
	main, err := a.sup.Main(p.Slug)
	if err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: project %s: %w", p.Slug, err))
		return
	}
	// An archived project is reopened by talking to it, which is what
	// archiveProject promises. Send refuses an archived session, and
	// nothing on the project page unarchives one: without this the only
	// way back into a project is finding its main thread in the
	// sidebar's archived list.
	if a.sup.Meta(main).Archived {
		if err := a.sup.SetArchived(main, false); err != nil {
			writeErr(w, statusFor(err), fmt.Errorf("serve: api: project %s: reopen: %w", p.Slug, err))
			return
		}
	}
	// Send refuses on an armed ask, but as a plain error statusFor would
	// call a 500. The page has to be able to say which question is in
	// the way, so the refusal comes back as a conflict carrying it.
	if ask := a.sup.PendingAsk(main); ask != nil {
		writeErr(w, http.StatusConflict, fmt.Errorf("serve: api: %s: a pending ask (%s) takes the next line; answer it first", main, ask.Text))
		return
	}
	if err := a.sup.Send(main, body.Text); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: project %s: %w", p.Slug, err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "main": main})
}

// archiveProject stops the project — main, every thread, and the
// container each of them runs — and folds their conversations away.
// Nothing on disk is deleted: the definition, MEMORY.md and every
// transcript stay, and a message to the project starts it again.
func (a *API) archiveProject(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	main := a.sup.MainID(p.Slug)
	ids := a.sup.projectSessions(p.Slug, main)
	if err := a.sup.EndProject(p.Slug); err != nil {
		writeErr(w, statusFor(err), fmt.Errorf("serve: api: archive project %s: %w", p.Slug, err))
		return
	}
	if main != "" {
		ids = append(ids, main)
	}
	for _, id := range ids {
		// A queued thread that was dropped is gone for good: stopChild
		// removed its row and it never wrote a history file. Anything
		// else is archived even if its first entry has not landed yet —
		// a thread that started a moment ago is still the project's.
		if !a.sup.historyExists(id) && a.sup.Meta(id) == (SessionMeta{}) {
			continue
		}
		if err := a.sup.SetArchived(id, true); err != nil {
			writeErr(w, statusFor(err), fmt.Errorf("serve: api: archive project %s: %w", p.Slug, err))
			return
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "archived": len(ids)})
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
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"project": p})
}

func (a *API) renameProject(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	var body struct {
		Name string `json:"name"`
	}
	if !decode(w, r, &body) {
		return
	}
	if err := a.sup.RenameProject(p.Slug, body.Name); err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// deleteProject removes the definition directory and everything keyed by
// its slug. The confirm that names the files lives in the web; by the
// time this is called the person has typed the slug back.
func (a *API) deleteProject(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	if err := a.sup.DeleteProject(p.Slug); err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// assignProject moves one session into a project by slug; an empty slug
// takes it out of whichever it was in. A project session is refused: its
// project is recorded in its history, not here.
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
		if errors.Is(err, ErrUnknownProject) && oldProjectID(body.Project) {
			writeErr(w, http.StatusBadRequest, errOldProjectID)
			return
		}
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

	// A work area is a definition directory from the start, with no repos
	// in it: the repos it works on are added by hand on the project page,
	// because the checkouts these sessions happened to use are not the
	// same list.
	p, err := a.sup.NewProject(name)
	if err != nil {
		writeErr(w, statusFor(err), err)
		return
	}
	// A session that vanished between listing and assigning is not a
	// reason to fail the whole group; report how many actually moved.
	moved := 0
	for _, id := range ids {
		if err := a.sup.AssignProject(id, p.Slug); err == nil {
			moved++
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"project": p, "moved": moved})
}
