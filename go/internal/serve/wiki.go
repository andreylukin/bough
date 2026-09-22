package serve

// The wiki surface of the control room: ~/.bough/wiki read as pages,
// claims and their evidence, plus the few writes a review needs. The
// model lives in plugins/wiki; this is the wire.

import (
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"time"

	"github.com/andreylukin/bough/plugins/wiki"
)

func (a *API) wikiStore() *wiki.Store { return wiki.Open(a.home) }

// wikiErr maps the store's sentinels onto HTTP.
func wikiErr(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, wiki.ErrNotFound):
		writeErr(w, http.StatusNotFound, err)
	case errors.Is(err, wiki.ErrBadPath):
		writeErr(w, http.StatusBadRequest, err)
	case errors.Is(err, wiki.ErrStale):
		writeErr(w, http.StatusConflict, err)
	case errors.Is(err, wiki.ErrNoProfile):
		writeErr(w, http.StatusConflict, err)
	default:
		writeErr(w, http.StatusInternalServerError, err)
	}
}

func (a *API) wikiIndex(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, a.wikiStore().Index(time.Now()))
}

func (a *API) wikiPage(w http.ResponseWriter, r *http.Request) {
	pg, err := a.wikiStore().Page(r.URL.Query().Get("path"))
	if err != nil {
		wikiErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, pg)
}

func (a *API) putWikiPage(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Path string `json:"path"`
		Body string `json:"body"`
	}
	if !decode(w, r, &body) {
		return
	}
	if err := a.wikiStore().WritePage(body.Path, body.Body); err != nil {
		wikiErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) wikiHistory(w http.ResponseWriter, r *http.Request) {
	cs, err := a.wikiStore().History(r.URL.Query().Get("path"))
	if err != nil {
		wikiErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"commits": cs})
}

func (a *API) wikiSource(w http.ResponseWriter, r *http.Request) {
	seq, err := intParam(r, "seq")
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	src, err := a.wikiStore().Source(r.URL.Query().Get("session"), seq)
	if err != nil {
		wikiErr(w, fmt.Errorf("serve: api: no entry %s#%d: %w", r.URL.Query().Get("session"), seq, err))
		return
	}
	writeJSON(w, http.StatusOK, src)
}

func (a *API) wikiReview(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, a.wikiStore().Review(time.Now()))
}

// wikiClaim applies one review decision. The client sends the lines it
// was shown; an ingest that moved them since is a 409, not an edit to
// whatever now sits at that line.
func (a *API) wikiClaim(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Path   string `json:"path"`
		Line   int    `json:"line"`
		End    int    `json:"end"`
		Raw    string `json:"raw"`
		Action string `json:"action"`
	}
	if !decode(w, r, &body) {
		return
	}
	if body.Action != "inference" && body.Action != "drop" {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown claim action %q", body.Action))
		return
	}
	if err := a.wikiStore().EditClaim(body.Path, body.Line, body.End, body.Raw, body.Action); err != nil {
		wikiErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func (a *API) wikiActivity(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, a.wikiStore().Activity(time.Now()))
}

func (a *API) wikiCheck(w http.ResponseWriter, r *http.Request) {
	probs, err := a.wikiStore().Problems()
	if err != nil {
		writeErr(w, http.StatusInternalServerError, err)
		return
	}
	out := make([]map[string]any, 0, len(probs))
	for _, p := range probs {
		out = append(out, map[string]any{"page": p.Page, "line": p.Line, "msg": p.Msg})
	}
	writeJSON(w, http.StatusOK, map[string]any{"problems": out})
}

func (a *API) wikiSearch(w http.ResponseWriter, r *http.Request) {
	limit := 8
	if n, err := strconv.Atoi(r.URL.Query().Get("limit")); err == nil && n > 0 && n <= 50 {
		limit = n
	}
	writeJSON(w, http.StatusOK, map[string]any{"hits": a.wikiStore().Search(r.URL.Query().Get("q"), limit)})
}

// wikiIngest starts one scheduler tick now. It answers as soon as the
// run has started: an ingest takes minutes, and Activity shows it.
func (a *API) wikiIngest(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Session string `json:"session"`
	}
	if r.ContentLength != 0 && !decode(w, r, &body) {
		return
	}
	if err := a.ingest(body.Session); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: start ingest: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// me is GET /api/me: the brief and signals the Me page renders.
func (a *API) me(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, a.wikiStore().Me(time.Now()))
}

// meRefresh is POST /api/me/refresh: write today's brief now.
func (a *API) meRefresh(w http.ResponseWriter, r *http.Request) {
	if err := a.brief(); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: start brief: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

// meTriage is POST /api/me/triage: dismiss, undismiss, pin or unpin one
// row by its key; a dismissal may also teach a rule, a sentence filed
// under Not mine in the profile.
func (a *API) meTriage(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Action string `json:"action"`
		Key    string `json:"key"`
		Rule   string `json:"rule"`
	}
	if !decode(w, r, &body) {
		return
	}
	st := a.wikiStore()
	t, err := st.Mark(body.Action, body.Key, time.Now())
	if err != nil {
		writeErr(w, http.StatusBadRequest, err)
		return
	}
	if body.Rule != "" {
		if err := st.Rule(body.Rule, time.Now()); err != nil {
			wikiErr(w, err)
			return
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "triage": t})
}

// meSteer is POST /api/me/steer: a sentence for the brief, filed under
// Watch or Not mine in the profile; the answer says which.
func (a *API) meSteer(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Text string `json:"text"`
	}
	if !decode(w, r, &body) {
		return
	}
	section, err := a.wikiStore().Steer(body.Text, time.Now())
	if err != nil {
		wikiErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "section": section})
}

// spawnIngest runs `bough wiki run` detached, appending to the same
// log the scheduler writes. The run takes the wiki's lock, so a tick
// that is already ingesting makes this a no-op rather than a second run.
func (a *API) spawnIngest(only string) error {
	args := []string{"wiki", "run"}
	if only != "" {
		args = append(args, "--only", only)
	}
	return a.spawnWiki(args...)
}

// spawnBrief runs `bough wiki brief` detached, the same way.
func (a *API) spawnBrief() error { return a.spawnWiki("wiki", "brief") }

func (a *API) spawnWiki(args ...string) error {
	exe := a.sup.opt.Exe
	if exe == "" {
		var err error
		if exe, err = os.Executable(); err != nil {
			return err
		}
	}
	dir := a.wikiStore().Dir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	logf, err := os.OpenFile(filepath.Join(dir, "ingest.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	cmd := exec.Command(exe, args...)
	cmd.Dir = dir
	cmd.Stdout, cmd.Stderr = logf, logf
	if err := cmd.Start(); err != nil {
		logf.Close()
		return err
	}
	go func() { _ = cmd.Wait(); logf.Close() }()
	return nil
}
