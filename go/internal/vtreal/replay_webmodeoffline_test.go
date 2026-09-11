package vtreal

// Surface "web-mode-offline": the web row on an ephemeral port with
// the artifacts row and the attention board mounted on it, no network
// beyond loopback. The pages must answer during a replayed turn, the
// port must be released when bough quits, a second boot must bind the
// same port, and nothing the server logs may land on the TUI.

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// webModeOfflinePort reserves a free loopback port and releases it so
// the web row can bind it; the port is the same across both boots.
func webModeOfflinePort(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close()
	return addr
}

func webModeOfflineConfig(tape, addr string) string {
	// replayConfig disables attention; drop that stanza so the live
	// one below does not clash on the id.
	base := strings.Replace(replayConfig(tape), "- id: attention\n  plugin: attention\n  disabled: true\n", "", 1)
	return base + fmt.Sprintf(`
- id: web
  plugin: web
  config: {addr: %q}
- id: artifacts
  plugin: artifacts
  config: {open: false}
- id: graph
  plugin: graph
- id: attention
  plugin: attention
  config: {web: true}
`, addr)
}

func webModeOfflineGet(url string) (int, string, error) {
	c := &http.Client{Timeout: 3 * time.Second}
	resp, err := c.Get(url)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, string(b), nil
}

// webModeOfflineQuit presses ctrl+c twice and waits for exit.
func webModeOfflineQuit(t *testing.T, a *app) {
	t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatalf("bough did not exit after two ctrl+c:\n%s", a.text())
	}
}

func webModeOfflineBoot(t *testing.T, tape, addr string) *app {
	t.Helper()
	a := startCfg(t, 100, 30, webModeOfflineConfig(tape, addr))
	// A recorded artifact on disk: the pages read the store, not the process.
	dir := filepath.Join(a.home, ".bough", "artifacts", "rec")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "db-comparison.ui"), []byte("root = Card([title])\ntitle = TextContent(\"DB comparison\")\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

func TestWebModeOffline(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/artifacts-row.jsonl")
	addr := webModeOfflinePort(t)
	base := "http://" + addr
	a := webModeOfflineBoot(t, tape, addr)
	a.check("boot")

	endpoints := []string{"/artifacts/", "/artifacts/rec/db-comparison", "/artifacts/rec/db-comparison.ui", "/api/board", "/"}

	t.Run("TestWebModeOfflineEndpointsDuringTurn", func(t *testing.T) {
		a.typeText("publish a db comparison page")
		a.key(uv.KeyEnter, 0)
		// Hit every endpoint while the turn replays, until it is done.
		deadline := time.Now().Add(30 * time.Second)
		for {
			for _, p := range endpoints {
				code, body, err := webModeOfflineGet(base + p)
				if err != nil {
					t.Fatalf("GET %s: %v", p, err)
				}
				if code != http.StatusOK {
					t.Fatalf("GET %s: status %d: %.300s", p, code, body)
				}
			}
			if a.doneCount() >= 1 {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("turn never finished:\n%s", a.text())
			}
		}
		if _, body, _ := webModeOfflineGet(base + "/artifacts/rec/db-comparison.ui"); !strings.Contains(body, "DB comparison") {
			t.Errorf("raw artifact body wrong: %.300s", body)
		}
		a.check("after turn")
	})
	t.Run("TestWebModeOfflineNoLogsOnScreen", func(t *testing.T) {
		// Odd requests must not make the server write over the alt screen.
		_, _, _ = webModeOfflineGet(base + "/artifacts/../../etc/passwd")
		_, _, _ = webModeOfflineGet(base + "/api/detail?x=%zz")
		_, _, _ = webModeOfflineGet(base + "/artifacts/nosuch/page")
		s := a.settled()
		// "http: " with the space is net/http's log prefix; the
		// transcript's own "http://" link must not match.
		for _, bad := range []string{"web: ", "http: ", "listen tcp"} {
			if strings.Contains(s, bad) {
				t.Errorf("server log %q on the TUI:\n%s", bad, s)
			}
		}
		a.check("after bad requests")
	})
	t.Run("TestWebModeOfflinePortReleasedOnExit", func(t *testing.T) {
		webModeOfflineQuit(t, a)
		if _, _, err := webModeOfflineGet(base + "/artifacts/"); err == nil {
			t.Errorf("%s still answers after bough exited", addr)
		}
		ln, err := net.Listen("tcp", addr)
		if err != nil {
			t.Fatalf("port not released after exit: %v", err)
		}
		_ = ln.Close()
	})
	t.Run("TestWebModeOfflineRestartSamePort", func(t *testing.T) {
		b := webModeOfflineBoot(t, tape, addr)
		b.check("second boot")
		if s := b.text(); strings.Contains(s, "address already in use") {
			t.Fatalf("EADDRINUSE on restart:\n%s", s)
		}
		for _, p := range endpoints {
			code, body, err := webModeOfflineGet(base + p)
			if err != nil || code != http.StatusOK {
				t.Fatalf("GET %s after restart: %d %v %.300s", p, code, err, body)
			}
		}
		webModeOfflineQuit(t, b)
	})
}
