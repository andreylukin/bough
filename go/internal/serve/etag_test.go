package serve

import (
	"net/http"
	"testing"
)

// A poll that already holds the body gets a 304 with no bytes; a stale tag gets the body.
func TestPolledReadsAnswer304(t *testing.T) {
	f := newAPI(t)
	for _, path := range []string{"/api/sessions", "/api/hooks"} {
		get := func(tag string) *http.Response {
			req, _ := http.NewRequest("GET", f.srv.URL+path, nil)
			if tag != "" {
				req.Header.Set("If-None-Match", tag)
			}
			resp, err := f.srv.Client().Do(req)
			if err != nil {
				t.Fatal(err)
			}
			resp.Body.Close()
			return resp
		}
		first := get("")
		tag := first.Header.Get("ETag")
		if first.StatusCode != http.StatusOK || tag == "" {
			t.Fatalf("%s: first = %d, etag %q", path, first.StatusCode, tag)
		}
		if again := get(tag); again.StatusCode != http.StatusNotModified {
			t.Fatalf("%s: with its tag = %d, want 304", path, again.StatusCode)
		}
		if stale := get(`"stale"`); stale.StatusCode != http.StatusOK {
			t.Fatalf("%s: stale tag = %d, want 200", path, stale.StatusCode)
		}
	}
}
