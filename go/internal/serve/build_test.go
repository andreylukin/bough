package serve

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

// The control room's UI ships inside the binary, so a page opened before
// `bough update` keeps running the old bundle against a new server and
// the update looks like it did not land. Every answer names its build so
// the page can notice.
func TestEveryAnswerNamesItsBuild(t *testing.T) {
	f := newAPI(t)
	for _, path := range []string{"/api/health", "/api/sessions"} {
		req, err := http.NewRequest("GET", f.srv.URL+path, nil)
		if err != nil {
			t.Fatal(err)
		}
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if got := res.Header.Get(BuildHeader); got == "" {
			t.Errorf("%s answered with no %s", path, BuildHeader)
		}
	}
}

// A refusal is still an answer from this build: a page must be able to
// tell "restarted onto a new build" from "the server went away", and
// Guard answers before the API ever sees the request.
func TestRefusalsNameTheirBuildToo(t *testing.T) {
	f := newAPI(t)
	guarded := httptest.NewServer(Guard(f.api, "tok", false))
	t.Cleanup(guarded.Close)

	req, err := http.NewRequest("GET", guarded.URL+"/api/sessions", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Host = "example.com" // not loopback: Guard refuses before the API
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusForbidden {
		t.Fatalf("a non-loopback host got %d, want 403", res.StatusCode)
	}
	if got := res.Header.Get(BuildHeader); got == "" {
		t.Errorf("a 403 answered with no %s", BuildHeader)
	}
}

func TestBuildIDIsStable(t *testing.T) {
	if buildID() == "" {
		t.Fatal("buildID is empty")
	}
	if buildID() != buildID() {
		t.Fatal("buildID changed between calls")
	}
}
