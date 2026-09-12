package serve

// The off switch, writable. Everything bough discovers is on by
// default and ~/.bough/off.yml is the one file that turns a thing off;
// this is the same file the user edits by hand, so the writer in
// plugins/offlist (which refuses to rewrite a file it cannot parse)
// does the work.

import (
	"fmt"
	"net/http"
	"slices"
	"strings"
)

// offKinds are the things that have an off switch. An unknown kind is
// a typo in the caller, not a new feature: rejecting it keeps off.yml
// from filling with entries nothing ever reads.
var offKinds = []string{"skill", "hook", "watcher", "rule", "plugin"}

// setOff answers POST /api/off.
func (a *API) setOff(w http.ResponseWriter, r *http.Request) {
	var body struct {
		ID  string `json:"id"`
		Off bool   `json:"off"`
	}
	if !decode(w, r, &body) {
		return
	}
	kind, id, ok := strings.Cut(strings.TrimSpace(body.ID), ":")
	if !ok || id == "" {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: %q is not a \"<kind>:<id>\" pair", body.ID))
		return
	}
	if !slices.Contains(offKinds, kind) {
		writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown kind %q, want one of %s", kind, strings.Join(offKinds, ", ")))
		return
	}
	if err := a.off().Set(a.boughHome(), kind, id, body.Off); err != nil {
		writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: write off list: %w", err))
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}
