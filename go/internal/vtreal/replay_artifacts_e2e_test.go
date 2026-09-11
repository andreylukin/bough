package vtreal

// Artifacts end to end on the real binary: a replayed model whose
// blocks call tools.artifact*, run by the REAL codemode row, so the
// page is stored, served and opened for real. No network (the web row
// binds 127.0.0.1:0, never the user's :7683) and no browser (a stub
// `open`/`xdg-open` on PATH records the URLs it was handed).

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"
)

const e2eProgram = `root = Card([head, tbl, ask])
head = CardHeader("Smoke page", "e2e")
tbl = Table([Col("engine", ["SQLite", "DuckDB"]), Col("p99 ms", [1.8, 14.2], "number")])
ask = Buttons([Button("Keep SQLite"), Button("Try DuckDB")])`

// e2eTape writes a tape of turns; each turn is the model's replies in
// order (a js block, then its stop).
func e2eTape(t *testing.T, turns ...[]string) string {
	t.Helper()
	dir := t.TempDir()
	var b strings.Builder
	seq := 0
	line := func(kind string, data map[string]any) {
		seq++
		j, _ := json.Marshal(map[string]any{"seq": seq, "at": "2026-09-11T10:00:00Z", "kind": kind, "data": data})
		b.Write(j)
		b.WriteByte('\n')
	}
	line("meta", map[string]any{"cwd": dir})
	for i, replies := range turns {
		line("input", map[string]any{"text": fmt.Sprintf("turn %d", i+1)})
		for _, r := range replies {
			line("assistant", map[string]any{"text": r})
		}
		line("done", map[string]any{"text": ""})
	}
	p := filepath.Join(dir, "tape.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func jsBlock(code string) string { return "```js\n" + code + "\n```" }
func stopReply(s string) string  { return "```stop\n" + s + "\n```" }

// e2eConfig is the replay overlay with the real codemode row and the
// web row on addr.
func e2eConfig(t *testing.T, tape, addr string) string {
	t.Helper()
	yml := replayConfig(tape)
	replayCode := fmt.Sprintf("- id: codemode\n  plugin: replay\n  config: {file: %q, provide: codemode}\n", tape)
	if !strings.Contains(yml, replayCode) {
		t.Fatal("replayConfig changed shape: its codemode row was not found")
	}
	yml = strings.Replace(yml, replayCode, "- id: codemode\n  plugin: codemode\n", 1)
	return yml + fmt.Sprintf("- id: web\n  plugin: web\n  config: {addr: %q}\n", addr)
}

// hlProc is a `bough --headless` whose stdin stays open, so the test
// can talk to it turn by turn and an idle agent can be woken.
type hlProc struct {
	t     *testing.T
	stdin io.WriteCloser
	mu    sync.Mutex
	out   strings.Builder
	err   strings.Builder
}

// startHeadless boots bough in home with yml; the stub opener appends
// every URL it is handed to openLog.
func startHeadless(t *testing.T, home, yml, openLog string) *hlProc {
	t.Helper()
	cfg := filepath.Join(t.TempDir(), "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	stubs := t.TempDir()
	stub := "#!/bin/sh\necho \"$1\" >> \"$OPEN_LOG\"\n"
	for _, n := range []string{"open", "xdg-open"} {
		if err := os.WriteFile(filepath.Join(stubs, n), []byte(stub), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	cmd := exec.Command(bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "PATH="+stubs+string(os.PathListSeparator)+os.Getenv("PATH"),
		"OPEN_LOG="+openLog, "NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	p := &hlProc{t: t}
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	p.stdin = stdin
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stderr = writerFunc(func(b []byte) (int, error) {
		p.mu.Lock()
		defer p.mu.Unlock()
		return p.err.Write(b)
	})
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	go func() {
		sc := bufio.NewScanner(stdout)
		sc.Buffer(make([]byte, 1<<20), 1<<24)
		for sc.Scan() {
			p.mu.Lock()
			p.out.WriteString(sc.Text() + "\n")
			p.mu.Unlock()
		}
	}()
	t.Cleanup(func() {
		_ = stdin.Close()
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})
	return p
}

type writerFunc func([]byte) (int, error)

func (f writerFunc) Write(b []byte) (int, error) { return f(b) }

func (p *hlProc) screen() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return "--- stdout ---\n" + p.out.String() + "--- stderr ---\n" + p.err.String()
}

func (p *hlProc) stdout() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.out.String()
}

func (p *hlProc) send(line string) {
	p.t.Helper()
	if _, err := io.WriteString(p.stdin, line+"\n"); err != nil {
		p.t.Fatal(err)
	}
}

// waitFor polls stdout until ok holds, and returns it.
func (p *hlProc) waitFor(what string, ok func(string) bool) string {
	p.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if s := p.stdout(); ok(s) {
			return s
		}
		time.Sleep(50 * time.Millisecond)
	}
	p.t.Fatalf("never saw %s:\n%s", what, p.screen())
	return ""
}

func (p *hlProc) waitDone(n int) string {
	p.t.Helper()
	return p.waitFor(fmt.Sprintf("%d [done]", n), func(s string) bool {
		return strings.Count("\n"+s, "\n[done]") >= n
	})
}

var artifactURLRe = regexp.MustCompile(`http://127\.0\.0\.1:(\d+)/artifacts/([^/\s"]+)/smoke\b`)

func httpGet(t *testing.T, u string) (int, string, string) {
	t.Helper()
	r, err := http.Get(u)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Body.Close()
	b, _ := io.ReadAll(r.Body)
	return r.StatusCode, r.Header.Get("Content-Type"), string(b)
}

// opened is what the stub opener was handed, once at least n URLs
// arrived or after a grace period (the opener runs detached).
func opened(t *testing.T, log string, n int, wait time.Duration) []string {
	t.Helper()
	deadline := time.Now().Add(wait)
	for {
		b, err := os.ReadFile(log)
		if err != nil && !os.IsNotExist(err) {
			t.Fatal(err)
		}
		got := strings.Fields(string(b))
		if len(got) >= n || time.Now().After(deadline) {
			return got
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func TestArtifactsE2E(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	publish := jsBlock(fmt.Sprintf("console.log(tools.artifact(\"smoke\", %q))", e2eProgram))
	tape := e2eTape(t,
		[]string{jsBlock(`try { tools.artifact("broken", "just prose, not a program") } catch (e) { console.log("REFUSED " + e) }`), stopReply("that failed")},
		[]string{publish, stopReply("Published smoke.")},
		[]string{publish, stopReply("Republished smoke.")},
		[]string{jsBlock(`console.log(tools.artifactPatch("smoke", "head = CardHeader(\"Smoke v2\", \"patched\")"))`), stopReply("Patched.")},
		// The wake: an answer on the page starts this turn by itself.
		[]string{jsBlock(`console.log("ANSWERS " + tools.artifactAnswers("smoke"))`), stopReply("The user chose SQLite.")},
	)
	openLog := filepath.Join(home, "opened.log")
	p := startHeadless(t, home, e2eConfig(t, tape, "127.0.0.1:0"), openLog)

	// Bad OpenUI: the tool refuses with the fix in the message, and
	// nothing is stored.
	p.send("publish something broken")
	out := p.waitDone(1)
	if !strings.Contains(out, "REFUSED") || !strings.Contains(out, "no statements") || !strings.Contains(out, "tools.artifactGuide()") {
		t.Fatalf("bad program: no structured refusal in the result:\n%s", p.screen())
	}

	p.send("publish the smoke page")
	out = p.waitDone(2)
	m := artifactURLRe.FindStringSubmatch(out)
	if m == nil {
		t.Fatalf("result block carries no artifact URL:\n%s", p.screen())
	}
	url, port, sess := m[0], m[1], m[2]
	if port == "7683" {
		t.Fatalf("test bound the user's port: %s", url)
	}
	dir := filepath.Join(home, ".bough", "artifacts", sess)
	if b, err := os.ReadFile(filepath.Join(dir, "smoke.ui")); err != nil || strings.TrimSpace(string(b)) != e2eProgram {
		t.Fatalf("smoke.ui in %s: %v %q", dir, err, b)
	}
	if _, err := os.Stat(filepath.Join(dir, "broken.ui")); err == nil {
		t.Error("a refused program was stored")
	}
	if got := opened(t, openLog, 1, 10*time.Second); len(got) != 1 || got[0] != url {
		t.Fatalf("first publish opened %q, want exactly [%s]", got, url)
	}

	base := "http://127.0.0.1:" + port + "/artifacts/"
	t.Run("Serves", func(t *testing.T) {
		code, ct, body := httpGet(t, url)
		if code != 200 || !strings.HasPrefix(ct, "text/html") || !strings.Contains(body, "const PAGE = {") || !strings.Contains(body, "openui-bundle.min.js") || !strings.Contains(body, "Smoke page") {
			t.Errorf("viewer: %d %q %.300s", code, ct, body)
		}
		code, ct, body = httpGet(t, url+".ui")
		if code != 200 || !strings.HasPrefix(ct, "text/plain") || strings.TrimSpace(body) != e2eProgram {
			t.Errorf("program: %d %q %q", code, ct, body)
		}
		code, ct, body = httpGet(t, base+"_lib/openui-bundle.min.js")
		if code != 200 || ct != "application/javascript" || !strings.Contains(body, "__OpenUI") {
			t.Errorf("bundle: %d %q", code, ct)
		}
		if code, ct, _ = httpGet(t, base+"_lib/openui-styles.css"); code != 200 || ct != "text/css" {
			t.Errorf("styles: %d %q", code, ct)
		}
		if code, _, body = httpGet(t, base); code != 200 || !strings.Contains(body, "/artifacts/"+sess+"/smoke") {
			t.Errorf("index: %d %.300s", code, body)
		}
		if code, _, _ = httpGet(t, base+sess+"/nope"); code != 404 {
			t.Errorf("unknown page = %d", code)
		}
	})

	// Republish: same URL, no second browser tab.
	p.send("publish it again")
	out = p.waitDone(3)
	if strings.Count(out, url) < 2 {
		t.Errorf("republish result carries no URL:\n%s", p.screen())
	}
	if got := opened(t, openLog, 2, time.Second); len(got) != 1 {
		t.Errorf("republish opened the browser again: %q", got)
	}

	// Patch: the served program changes and the events stream says so.
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	req, _ := http.NewRequestWithContext(ctx, "GET", url+"/events", nil)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if ct := resp.Header.Get("Content-Type"); ct != "text/event-stream" {
		t.Errorf("events content type %q", ct)
	}
	events := make(chan string, 1)
	go func() {
		var got string
		buf := make([]byte, 256)
		for !strings.Contains(got, "event: update") {
			n, err := resp.Body.Read(buf)
			got += string(buf[:n])
			if err != nil {
				break
			}
		}
		events <- got
	}()
	time.Sleep(1100 * time.Millisecond) // past the stream's first mtime read
	p.send("patch the header")
	p.waitDone(4)
	if _, _, body := httpGet(t, url+".ui"); !strings.Contains(body, `head = CardHeader("Smoke v2", "patched")`) || !strings.Contains(body, "tbl = Table(") {
		t.Errorf("patched program: %q", body)
	}
	select {
	case got := <-events:
		if !strings.Contains(got, "event: update") {
			t.Errorf("events after patch: %q", got)
		}
	case <-ctx.Done():
		t.Error("events stream never announced the patch")
	}

	// An answer posted by the page wakes the idle agent, which reads it.
	r, err := http.Post(url+"/answers", "application/json", strings.NewReader(
		`{"kind":"action","value":{"type":"continue_conversation","message":"Keep SQLite"},"state":{"pick":"sqlite"}}`))
	if err != nil {
		t.Fatal(err)
	}
	r.Body.Close()
	if r.StatusCode != 200 {
		t.Fatalf("answers POST = %d", r.StatusCode)
	}
	out = p.waitDone(5)
	if !strings.Contains(out, "ANSWERS {") || !strings.Contains(out, `"pick": "sqlite"`) || !strings.Contains(out, `"message": "Keep SQLite"`) {
		t.Errorf("the woken turn did not read the answer with tools.artifactAnswers:\n%s", p.screen())
	}

	// /artifacts lists the page; /artifacts open opens the latest.
	p.send("/artifacts")
	p.waitFor("/artifacts listing", func(s string) bool { return strings.Contains(s, "[system] smoke (v3)  "+url) })
	p.send("/artifacts open")
	p.waitFor("/artifacts open", func(s string) bool { return strings.Contains(s, "[system] opened "+url) })
	if got := opened(t, openLog, 2, 10*time.Second); len(got) != 2 || got[1] != url {
		t.Errorf("/artifacts open handed the browser %q, want a second %s", got, url)
	}

	// A second bough on the same HOME and port: its bind fails, and its
	// URLs are served by the first process — its own page included.
	second := strings.Replace(e2eProgram, "Smoke page", "Second page", 1)
	tape2 := e2eTape(t, []string{jsBlock(fmt.Sprintf("console.log(tools.artifact(\"smoke\", %q))", second)), stopReply("done")})
	openLog2 := filepath.Join(home, "opened2.log")
	p2 := startHeadless(t, home, e2eConfig(t, tape2, "127.0.0.1:"+port), openLog2)
	p2.send("publish from the second process")
	m2 := artifactURLRe.FindStringSubmatch(p2.waitDone(1))
	if m2 == nil || m2[1] != port || m2[2] == sess {
		t.Fatalf("second process URL %q (first %s):\n%s", m2, url, p2.screen())
	}
	if code, _, body := httpGet(t, m2[0]); code != 200 || !strings.Contains(body, "Second page") {
		t.Errorf("second process's page via the shared port: %d %.200s", code, body)
	}
	if code, _, body := httpGet(t, url); code != 200 || !strings.Contains(body, "Smoke v2") {
		t.Errorf("first process's page after the second started: %d %.200s", code, body)
	}
	if got := opened(t, openLog2, 1, 10*time.Second); len(got) != 1 || got[0] != m2[0] {
		t.Errorf("second process opened %q, want [%s]", got, m2[0])
	}
}
