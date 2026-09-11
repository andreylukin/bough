package artifacts

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

// The viewer references only files the /artifacts/_lib/ route serves,
// so a page never loads half a renderer.
func TestViewerReferencesServedLib(t *testing.T) {
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	refs := regexp.MustCompile(`(?:src|href)="(/artifacts/_lib/[^"]+)"`).FindAllStringSubmatch(viewerHTML, -1)
	if len(refs) < 2 {
		t.Fatalf("viewer references %d _lib files, want the bundle and the styles", len(refs))
	}
	for _, m := range refs {
		if code, body := get(t, s.web.URL()+m[1]); code != 200 || len(body) == 0 {
			t.Errorf("%s: %d (%d bytes)", m[1], code, len(body))
		}
	}
}

// Published pages really render in a browser: the OpenUI renderer
// draws the program's text into #root, reports no errors, and the
// page raises no uncaught exceptions. Drives Chrome through the
// agent-browser CLI (a page holds its /events stream open, so plain
// `chrome --dump-dom` never finishes loading). BOUGH_ARTIFACT_RENDER=1.
func TestPagesRenderInChrome(t *testing.T) {
	if os.Getenv("BOUGH_ARTIFACT_RENDER") != "1" {
		t.Skip("set BOUGH_ARTIFACT_RENDER=1")
	}
	ab, err := exec.LookPath("agent-browser")
	if err != nil {
		t.Skip("agent-browser not on PATH")
	}
	session := fmt.Sprintf("bough-render-%d", os.Getpid())
	run := func(args ...string) (string, error) {
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		out, err := exec.CommandContext(ctx, ab, append([]string{"--session", session}, args...)...).CombinedOutput()
		return strings.TrimSpace(string(out)), err
	}
	if out, err := run("open", "about:blank"); err != nil {
		t.Skipf("no Chrome for agent-browser: %v %s", err, out)
	}
	defer run("close")
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	pages := []struct {
		name, code string
		want       []string
	}{
		{"table", sample, []string{"DB comparison", "DuckDB", "Keep SQLite"}},
		{"chart", `root = Card([head, chart])
head = CardHeader("Latency trend", "last three releases")
chart = BarChart(["v1", "v2", "v3"], [Series("p99 ms", [14, 9, 6])])
`, []string{"Latency trend", "last three releases"}},
		{"form", `root = Card([head, form])
head = CardHeader("Deploy request", "fill and submit")
form = Form("deploy", btns, [who])
who = FormControl("Service name", Input("service", "e.g. api"))
btns = Buttons([Button("Submit request")])
`, []string{"Deploy request", "Service name", "Submit request"}},
	}
	dir := t.TempDir()
	for _, p := range pages {
		url, err := s.Publish(p.name, p.code)
		if err != nil {
			t.Fatal(err)
		}
		t.Run(p.name, func(t *testing.T) {
			_, _ = run("errors", "--clear")
			_, _ = run("console", "--clear")
			if out, err := run("open", url); err != nil {
				t.Fatalf("open: %v %s", err, out)
			}
			if out, err := run("wait", "#root *"); err != nil {
				t.Fatalf("renderer drew nothing into #root: %v %s", err, out)
			}
			_, _ = run("wait", "1000") // let charts and the errors POST settle
			// The program's source is inlined in PAGE; only #root proves the renderer drew it.
			drawn, err := run("eval", "document.getElementById('root').innerText")
			if err != nil {
				t.Fatalf("eval: %v %s", err, drawn)
			}
			for _, w := range p.want {
				if !strings.Contains(drawn, w) {
					t.Errorf("%q not drawn in #root: %s", w, drawn)
				}
			}
			if b, err := os.ReadFile(errorsPath(s.codePath(p.name))); err == nil {
				var es Errors
				if json.Unmarshal(b, &es) != nil || len(es.Errors) > 0 {
					t.Errorf("renderer reported errors: %s", b)
				}
			}
			if out, _ := run("errors"); strings.Contains(out, "Error") {
				t.Errorf("page errors: %s", out)
			}
			if out, _ := run("console"); strings.Contains(out, "Uncaught") {
				t.Errorf("console: %s", out)
			}
			shot := filepath.Join(dir, p.name+".png")
			if out, err := run("screenshot", "--full", shot); err != nil {
				t.Errorf("screenshot: %v %s", err, out)
			}
			t.Logf("screenshot: %s", shot)
		})
	}
}
