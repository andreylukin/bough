package serve

// The control room's static files: one HTML shell and one bundle,
// compiled into the binary so `bough serve` needs nothing on disk and
// no network. The bundle is built by hand (`bun run build` in web/)
// and committed, the same way plugins/artifacts ships its vendored
// viewer — that keeps `go build` the only build step this repo needs.

import (
	"bytes"
	"crypto/sha256"
	"embed"
	"encoding/hex"
	"io/fs"
	"net/http"
	"time"
)

//go:embed web/dist
var webFiles embed.FS

// asset is one embedded file and the tag a browser revalidates with.
type asset struct {
	body []byte
	etag string
}

// staticHandler serves the built UI. Unknown paths fall back to the
// shell rather than 404ing: the client owns its routes, and the API
// lives under /api, which the mux matches more specifically.
func staticHandler() (http.Handler, error) {
	sub, err := fs.Sub(webFiles, "web/dist")
	if err != nil {
		return nil, err
	}
	shell, err := fs.ReadFile(sub, "index.html")
	if err != nil {
		return nil, err
	}
	// Every asset is tagged with a hash of its own bytes. An embedded
	// file has a zero mod time, so http.FileServer sent the bundle with
	// no ETag, no Last-Modified and no Cache-Control — and a browser
	// given none of those caches on a heuristic of its own. The shell
	// is no-store and so always arrived fresh, which is what made this
	// hard to see: the page looked current while its JavaScript was
	// months old, and neither rebuilding nor restarting changed it.
	assets := map[string]asset{}
	err = fs.WalkDir(sub, ".", func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		b, err := fs.ReadFile(sub, p)
		if err != nil {
			return err
		}
		sum := sha256.Sum256(b)
		assets[p] = asset{body: b, etag: `"` + hex.EncodeToString(sum[:16]) + `"`}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			if a, ok := assets[r.URL.Path[1:]]; ok {
				// no-cache is "revalidate every time", not "do not
				// cache": with the ETag the answer is a 304 and no
				// bytes whenever the bundle has not changed.
				w.Header().Set("Cache-Control", "no-cache")
				w.Header().Set("ETag", a.etag)
				http.ServeContent(w, r, r.URL.Path, time.Time{}, bytes.NewReader(a.body))
				return
			}
			// No SPA fallback: this client has no client-side routes,
			// so a wrong path is a wrong path. Swallowing it into the
			// shell would turn every API typo into a silent 200.
			http.NotFound(w, r)
			return
		}
		// The shell is tiny and changes with every build; letting a
		// browser cache it is how you get a stale UI talking to a new
		// API after an update.
		w.Header().Set("Cache-Control", "no-store")
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write(shell)
	}), nil
}
