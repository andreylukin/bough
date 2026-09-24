package serve

// What a session is actually being told. Rules, AGENTS.md/CLAUDE.md
// and skills all depend on the directory being worked on, and the
// user runs every session from home — so this resolves against the
// session's own cwd, read from its history, never serve's.

import (
	"bytes"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"github.com/andreylukin/bough/plugins/contextmd"
	"github.com/andreylukin/bough/plugins/skills"
)

// A context file is prepended to EVERY turn, so length is a cost the
// user pays over and over. These are where a standing brief stops
// being one; the control room colours the count at the same numbers.
const (
	longLines    = 200
	tooLongLines = 400
)

// ContextFile is one AGENTS.md-style file: whether it is on disk, how
// long it is, and what the section de-duplication did to it.
// Dropped/Same are the only place bough says out loud that CLAUDE.md
// repeated AGENTS.md.
type ContextFile struct {
	Path string `json:"path"`
	// Lines is 0 for a file that is not there.
	Lines   int    `json:"lines"`
	Found   bool   `json:"found"`
	Dropped int    `json:"dropped"`
	Same    string `json:"same"`
}

// sessionContext answers GET /api/sessions/{id}/context.
func (a *API) sessionContext(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	in, ok := a.info(id)
	if !ok {
		writeErr(w, http.StatusNotFound, fmt.Errorf("serve: api: unknown session %q", id))
		return
	}
	cwd := in.Cwd
	cat := skills.DefaultFor(a.home, cwd).Catalog()
	if cat == nil {
		cat = []skills.SkillInfo{}
	}
	// A running child fixed its context-md paths at its start, and
	// filing it since changes only what the NEXT start reads: list what
	// the child reads, and name both so the page can say they differ.
	// With no child running, the next start is the one that counts.
	next := a.sessionSlug(id, in.Project)
	slug := next
	if in.Project == "" && a.sup.Live(id) {
		slug = a.sup.StartedIn(id)
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"cwd":          cwd,
		"rules":        a.ruleRows(cwd),
		"contextFiles": a.contextFiles(cwd, slug),
		"project":      slug,
		"nextProject":  next,
		"skills":       cat,
		// When this was read: the page keeps a snapshot while the files,
		// the filing and the child move on, and says how old it is.
		"readAt": time.Now().UTC().Format(time.RFC3339Nano),
	})
}

// sessionSlug is the project a session belongs to. A project session
// carries the slug in its own history meta, which cannot change; a
// local session is assigned one here, and that assignment can.
func (a *API) sessionSlug(id, fromHistory string) string {
	if fromHistory != "" {
		return fromHistory
	}
	return a.sup.Meta(id).Project
}

// contextFiles reports the files the context-md row reads, in the order
// it reads them, for a session in project slug ("" = none).
func (a *API) contextFiles(cwd, slug string) []ContextFile {
	// contextmd leaves AGENTS.md/CLAUDE.md relative because it re-reads
	// them against the session process's cwd every turn. serve is the
	// one caller that knows what that cwd is, so it joins them here.
	var paths []string
	for _, p := range contextmd.Paths(a.home, slug) {
		if !filepath.IsAbs(p) {
			p = filepath.Join(cwd, p)
		}
		paths = append(paths, p)
	}
	// Parts names only the files that contributed something, so a file
	// every section of which was already said by an earlier one is
	// absent from it — found, but with nothing left to report.
	parts := map[string]contextmd.Part{}
	for _, p := range contextmd.New(paths...).Parts() {
		parts[p.Path] = p
	}
	out := make([]ContextFile, 0, len(paths))
	for _, p := range paths {
		row := ContextFile{Path: p}
		if body, err := os.ReadFile(p); err == nil {
			row.Found, row.Lines = true, lineCount(body)
		}
		if part, ok := parts[p]; ok {
			row.Dropped, row.Same = part.Dropped, part.Same
		}
		out = append(out, row)
	}
	return out
}

// lineCount counts what an editor would show: a trailing newline ends
// the last line, it does not start an empty one.
func lineCount(body []byte) int {
	if len(body) == 0 {
		return 0
	}
	n := bytes.Count(body, []byte("\n"))
	if !bytes.HasSuffix(body, []byte("\n")) {
		n++
	}
	return n
}
