package serveclient

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writePid(t *testing.T, home string, pid int, addr string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "serve.pid"), fmt.Appendf(nil, "%d %s\t/\t\t\n", pid, addr), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestAddr(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Addr(home); !errors.Is(err, ErrNoServe) {
		t.Fatalf("missing = %v", err)
	}
	writePid(t, home, 0x7FFFFFFE, "127.0.0.1:1")
	if _, err := Addr(home); !errors.Is(err, ErrNoServe) {
		t.Fatalf("dead pid = %v", err)
	}
	if _, err := os.Stat(filepath.Join(home, ".bough", "serve.pid")); err != nil {
		t.Fatal("Addr removed the pidfile")
	}
	writePid(t, home, os.Getpid(), "127.0.0.1:7683")
	if got, err := Addr(home); err != nil || got != "http://127.0.0.1:7683" {
		t.Fatalf("Addr = %q %v", got, err)
	}
}

func TestClient(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == "POST" && r.URL.Path == "/api/sessions":
			var req ChildRequest
			json.NewDecoder(r.Body).Decode(&req)
			if req.SpawnedBy != "p" || req.Prompt != "task" || req.Slug != "demo" {
				w.WriteHeader(400)
				fmt.Fprintf(w, `{"error":"bad %+v"}`, req)
				return
			}
			w.WriteHeader(201)
			fmt.Fprint(w, `{"session":{"id":"c1"},"queued":true}`)
		case r.URL.Path == "/api/sessions/c1/agent" && r.URL.Query().Get("parent") == "p":
			fmt.Fprint(w, `{"status":"done","title":"t","reply":"r","project":"","spawnedBy":"p"}`)
		case r.URL.Path == "/api/sessions/c1/stop":
			var b map[string]string
			json.NewDecoder(r.Body).Decode(&b)
			if b["parent"] != "p" {
				w.WriteHeader(403)
				fmt.Fprint(w, `{"error":"not yours"}`)
				return
			}
			fmt.Fprint(w, `{"ok":true,"was":"running"}`)
		default:
			w.WriteHeader(404)
			fmt.Fprint(w, `{"error":"serve: api: unknown session"}`)
		}
	}))
	t.Cleanup(srv.Close)
	c := &Client{Base: srv.URL, Parent: "p"}
	ctx := context.Background()

	resp, err := c.CreateChild(ctx, ChildRequest{Prompt: "task", Slug: "demo", SpawnedBy: "p"})
	if err != nil || resp.Session != "c1" || !resp.Queued {
		t.Fatalf("CreateChild = %+v %v", resp, err)
	}
	st, err := c.Agent(ctx, "c1")
	if err != nil || st.Status != "done" || st.Reply != "r" {
		t.Fatalf("Agent = %+v %v", st, err)
	}
	if was, err := c.Stop(ctx, "c1"); err != nil || was != "running" {
		t.Fatalf("Stop = %q %v", was, err)
	}
	_, err = c.Agent(ctx, "zz")
	var ae *APIError
	if !errors.As(err, &ae) || ae.Code != 404 || !strings.Contains(ae.Msg, "unknown session") {
		t.Fatalf("404 = %v", err)
	}
}

// A LoadToken that finds the file another one has just created (O_EXCL)
// but not yet written waits for the write and returns that token: two
// serves starting at once, or a CLI beside a starting serve, used to
// fail "token file ... is empty".
func TestLoadTokenWaitsForTheCreatorsWrite(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := TokenPath(home)
	if err := os.MkdirAll(filepath.Dir(p), 0o700); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(p, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	type res struct {
		tok string
		err error
	}
	got := make(chan res, 1)
	go func() {
		tok, err := LoadToken(home)
		got <- res{tok, err}
	}()
	time.Sleep(200 * time.Millisecond)
	if _, err := f.WriteString("the-creators-token\n"); err != nil {
		t.Fatal(err)
	}
	if r := <-got; r.err != nil || r.tok != "the-creators-token" {
		t.Fatalf("LoadToken = %q, %v; want the creator's token", r.tok, r.err)
	}
}

// A client built from Addr sends the install's token as a Bearer header.
func TestClientSendsToken(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	tok, err := LoadToken(home)
	if err != nil || tok == "" {
		t.Fatalf("LoadToken = %q, %v", tok, err)
	}
	if again, _ := LoadToken(home); again != tok {
		t.Fatalf("second LoadToken = %q, want %q", again, tok)
	}
	if fi, err := os.Stat(TokenPath(home)); err != nil || fi.Mode().Perm() != 0o600 {
		t.Fatalf("token file = %v, %v", fi, err)
	}
	var got string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = r.Header.Get("Authorization")
		w.Write([]byte(`{}`))
	}))
	defer srv.Close()
	writePid(t, home, os.Getpid(), strings.TrimPrefix(srv.URL, "http://"))
	base, err := Addr(home)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := (&Client{Base: base}).Agent(context.Background(), "x"); err != nil {
		t.Fatal(err)
	}
	if got != "Bearer "+tok {
		t.Fatalf("Authorization = %q", got)
	}
}
