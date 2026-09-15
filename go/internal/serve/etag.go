package serve

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"strings"
)

// writeJSONTagged answers a polled read with an ETag: a client that
// already holds the same body gets a 304 and no bytes. Cache-Control
// no-cache makes the browser revalidate every poll on its own.
func writeJSONTagged(w http.ResponseWriter, r *http.Request, v any) {
	raw, err := json.Marshal(v)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, err)
		return
	}
	raw = append(raw, '\n')
	sum := sha256.Sum256(raw)
	tag := `"` + base64.RawURLEncoding.EncodeToString(sum[:16]) + `"`
	w.Header().Set("ETag", tag)
	w.Header().Set("Cache-Control", "no-cache")
	for _, t := range strings.Split(r.Header.Get("If-None-Match"), ",") {
		if t = strings.TrimPrefix(strings.TrimSpace(t), "W/"); t == tag || t == "*" {
			w.WriteHeader(http.StatusNotModified)
			return
		}
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(raw)
}
