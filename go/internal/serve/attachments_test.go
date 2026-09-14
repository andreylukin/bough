package serve

import (
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestAPIAttachments(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envNewID+"=created")
	f.api.home = t.TempDir()
	png := "\x89PNG\r\n\x1a\nfake"

	post := func(ctype, body string) (*http.Response, map[string]any) {
		resp, err := f.srv.Client().Post(f.srv.URL+"/api/attachments", ctype, strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var out map[string]any
		_ = json.NewDecoder(resp.Body).Decode(&out)
		return resp, out
	}

	resp, out := post("image/png", png)
	path, _ := out["path"].(string)
	if resp.StatusCode != http.StatusOK || filepath.Dir(path) != filepath.Join(f.api.home, ".bough", "attachments") || !strings.HasSuffix(path, ".png") {
		t.Fatalf("upload = %d %v", resp.StatusCode, out)
	}
	if b, _ := os.ReadFile(path); string(b) != png {
		t.Fatalf("saved %q", b)
	}

	get, err := f.srv.Client().Get(f.srv.URL + "/api/attachments?path=" + url.QueryEscape(path))
	if err != nil {
		t.Fatal(err)
	}
	b, _ := io.ReadAll(get.Body)
	get.Body.Close()
	if get.StatusCode != http.StatusOK || string(b) != png || get.Header.Get("Content-Type") != "image/png" {
		t.Fatalf("get = %d %q %s", get.StatusCode, b, get.Header.Get("Content-Type"))
	}

	if resp, _ := post("text/plain", "hi"); resp.StatusCode != http.StatusUnsupportedMediaType {
		t.Errorf("text upload = %d, want 415", resp.StatusCode)
	}
	// Any other file lands in the session's scratchpad under its own name.
	code, body := f.do(t, "POST", "/api/sessions", `{"cwd":"`+f.home+`","prompt":"hello"}`)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	id, _ := rowOf(t, body)["id"].(string)
	for _, want := range []string{"notes.pdf", "notes-2.pdf"} {
		resp, err := f.srv.Client().Post(f.srv.URL+"/api/sessions/"+id+"/files?name=..%2Fnotes.pdf", "application/pdf", strings.NewReader("pdf"))
		if err != nil {
			t.Fatal(err)
		}
		var o map[string]any
		_ = json.NewDecoder(resp.Body).Decode(&o)
		resp.Body.Close()
		if p, _ := o["path"].(string); resp.StatusCode != http.StatusOK || p != filepath.Join(f.api.home, ".bough", "scratch", id, want) {
			t.Fatalf("file upload = %d %v, want %s", resp.StatusCode, o, want)
		}
	}
	if resp, err := f.srv.Client().Post(f.srv.URL+"/api/sessions/nope/files?name=x", "text/plain", strings.NewReader("x")); err != nil || resp.StatusCode != http.StatusNotFound {
		t.Errorf("unknown session upload = %v %v, want 404", resp, err)
	}

	outside := filepath.Join(f.api.home, "secret.png")
	os.WriteFile(outside, []byte(png), 0o644)
	for _, p := range []string{outside, filepath.Join(filepath.Dir(path), "..", "..", "secret.png")} {
		if code, _ := f.do(t, "GET", "/api/attachments?path="+url.QueryEscape(p), ""); code != http.StatusNotFound {
			t.Errorf("GET %s = %d, want 404", p, code)
		}
	}
}
