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
	// MainArchived says the main thread is folded away: the session list
	// hides it, so the page could not otherwise tell an archived project
	// from a live one.
	MainArchived bool `json:"mainArchived,omitempty"`
	// MainOrb is the main thread's container as the orb strip shows it,
	// nil when it has never started one.
	MainOrb *OrbState `json:"mainOrb,omitempty"`
	// Orbs is every session orb recorded for this project, newest first
	// — one per session, main's included.
	Orbs []OrbState `json:"orbs"`
	// Threads is every session in the project but main, oldest first.
	Threads []Row `json:"threads"`
}

// inProject is the one membership rule, and the web's projectOf is its
// mirror: the page lists /api/projects/{slug}'s threads and the sidebar
// groups /api/sessions rows, and when the two rules differed the page
// showed threads the sidebar did not (main's children of another mode
// forced to running, children that died before writing history).
//
// row.Project is the slug the session's history recorded for a project
// session, and the one a person filed it under for a local one — both
// are membership. Parentage is not: a local child of main nests under
// main in the sidebar, not in the project. Archived threads are folded
// away, as listSessions hides them too.
func inProject(r Row, slug string) bool {
	return !r.Archived && r.Project == slug
}

// projectDetail is GET /api/projects/{slug}.
//
// Threads are every session inProject, parented to main or not: a
// session started with `bough --project <slug>`, or one from before the
// project had a main thread, has no parent and is still the person's
// work, though it sends no finish notices (see docs/orbs.md).
func (a *API) projectDetail(w http.ResponseWriter, r *http.Request) {
	p, ok := a.projectOr404(w, r.PathValue("slug"))
	if !ok {
		return
	}
	d := ProjectDetail{Project: p, Main: a.sup.MainID(p.Slug), Orbs: a.orbsOf(p.Slug), Threads: []Row{}}
	if d.Main != "" {
		d.MainArchived = a.sup.Meta(d.Main).Archived
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
		seen[in.ID] = true
		if row := a.row(in); inProject(row, p.Slug) {
			d.Threads = append(d.Threads, row)
		}
	}
	for _, row := range a.pendingRows(seen) {
		if inProject(row, p.Slug) {
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
	// reason to fail the whole group, nor is a project session (one whose
	// project was deleted is unassigned, and still refused): report what
	// moved and name what did not, and why.
	type refusal struct {
		ID    string `json:"id"`
		Error string `json:"error"`
	}
	moved, refused := 0, []refusal{}
	for _, id := range ids {
		if err := a.sup.AssignProject(id, p.Slug); err != nil {
			refused = append(refused, refusal{id, err.Error()})
			continue
		}
		moved++
	}
	writeJSON(w, http.StatusOK, map[string]any{"project": p, "moved": moved, "refused": refused})
}
