package serve

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

// The bundle was served with no ETag, no Last-Modified and no
// Cache-Control, so a browser cached it on a heuristic of its own and
// kept showing an old UI that neither a rebuild nor a restart could
// shift. Every asset must be revalidatable.
func TestBundleIsRevalidatable(t *testing.T) {
	t.Parallel()
	h, err := staticHandler()
	if err != nil {
		t.Fatal(err)
	}
	req := httptest.NewRequest("GET", "/app.js", nil)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /app.js = %d", rec.Code)
	}
	etag := rec.Header().Get("ETag")
	if etag == "" {
		t.Fatal("no ETag: a browser will cache the bundle on its own heuristic")
	}
	if cc := rec.Header().Get("Cache-Control"); cc != "no-cache" {
		t.Errorf("Cache-Control = %q, want no-cache", cc)
	}

	// The same tag must come back as a 304, or every load ships 500KB.
	req2 := httptest.NewRequest("GET", "/app.js", nil)
	req2.Header.Set("If-None-Match", etag)
	rec2 := httptest.NewRecorder()
	h.ServeHTTP(rec2, req2)
	if rec2.Code != http.StatusNotModified {
		t.Errorf("revalidation = %d, want 304", rec2.Code)
	}
}

// The shell must never be cached: it is what points at the bundle.
func TestShellIsNoStore(t *testing.T) {
	t.Parallel()
	h, err := staticHandler()
	if err != nil {
		t.Fatal(err)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest("GET", "/", nil))
	if cc := rec.Header().Get("Cache-Control"); cc != "no-store" {
		t.Errorf("shell Cache-Control = %q, want no-store", cc)
	}
}
