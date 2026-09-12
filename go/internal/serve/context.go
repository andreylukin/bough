package serve

// What a session is actually being told. Rules, AGENTS.md/CLAUDE.md
// and skills all depend on the directory being worked on, and the
// user runs every session from home — so this resolves against the
// session's own cwd, read from its history, never serve's.

import (
	"fmt"
	"net/http"
	"os"
	"path/filepath"

	"github.com/andreylukin/bough/plugins/contextmd"
	"github.com/andreylukin/bough/plugins/skills"
)

// ContextFile is one AGENTS.md-style file: whether it is on disk, and
// what the section de-duplication did to it. Dropped/Same are the only
// place bough says out loud that CLAUDE.md repeated AGENTS.md.
type ContextFile struct {
	Path    string `json:"path"`
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
	writeJSON(w, http.StatusOK, map[string]any{
		"cwd":          cwd,
		"rules":        a.ruleRows(cwd),
		"contextFiles": a.contextFiles(cwd),
		"skills":       cat,
	})
}

// contextFiles reports the four files the context-md row reads, in the
// order it reads them.
func (a *API) contextFiles(cwd string) []ContextFile {
	paths := []string{
		filepath.Join(cwd, "AGENTS.md"),
		filepath.Join(cwd, "CLAUDE.md"),
		filepath.Join(a.home, ".claude", "CLAUDE.md"),
		filepath.Join(a.home, ".bough", "BOUGH.md"),
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
		if _, err := os.Stat(p); err == nil {
			row.Found = true
		}
		if part, ok := parts[p]; ok {
			row.Dropped, row.Same = part.Dropped, part.Same
		}
		out = append(out, row)
	}
	return out
}
