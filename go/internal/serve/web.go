package serve

// The control room's static files: one HTML shell and one bundle,
// compiled into the binary so `bough serve` needs nothing on disk and
// no network. The bundle is built by hand (`bun run build` in web/)
// and committed, the same way plugins/artifacts ships its vendored
// viewer — that keeps `go build` the only build step this repo needs.

import (
	"embed"
	"io/fs"
	"net/http"
)

//go:embed web/dist
var webFiles embed.FS

// staticHandler serves the built UI. Unknown paths fall back to the
// shell rather than 404ing: the client owns its routes, and the API
// lives under /api, which the mux matches more specifically.
func staticHandler() (http.Handler, error) {
	sub, err := fs.Sub(webFiles, "web/dist")
	if err != nil {
		return nil, err
	}
	files := http.FileServer(http.FS(sub))
	shell, err := fs.ReadFile(sub, "index.html")
	if err != nil {
		return nil, err
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			if _, err := fs.Stat(sub, r.URL.Path[1:]); err == nil {
				files.ServeHTTP(w, r)
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
