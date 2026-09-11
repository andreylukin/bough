package vtreal

// Two browsers on one artifact page while a turn streams: the web row
// on a random port, both clients load the page and post button presses
// at once. Every press must reach the agent exactly once (as a "job"
// notice in history), in the order the answers log recorded them, with
// no 5xx; after quit the port is free again.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const webArtifactConcurrentBrowsersPresses = 6 // per client

var webArtifactConcurrentBrowsersURLRe = regexp.MustCompile(`http://127\.0\.0\.1:(\d+)/artifacts/`)
var webArtifactConcurrentBrowsersPressRe = regexp.MustCompile(`the user pressed "(c\d-p\d+)"`)

// webArtifactConcurrentBrowsersTape is one turn whose stop reply
// streams for several seconds (delay_ms per word), so the presses land
// mid-turn; the wakes after it run past the tape and stop cleanly.
func webArtifactConcurrentBrowsersTape(t *testing.T) string {
	t.Helper()
	reply := "```stop\n" + strings.Repeat("streaming ", 400) + "\n```"
	lines := []map[string]any{
		{"seq": 1, "at": "2026-09-10T10:00:00Z", "kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"seq": 2, "at": "2026-09-10T10:00:01Z", "kind": "input", "data": map[string]any{"text": "stream something long"}},
		{"seq": 3, "at": "2026-09-10T10:00:02Z", "kind": "assistant", "data": map[string]any{"text": reply}},
		{"seq": 4, "at": "2026-09-10T10:00:03Z", "kind": "done", "data": map[string]any{"text": ""}},
	}
	var b bytes.Buffer
	for _, l := range lines {
		j, _ := json.Marshal(l)
		b.Write(j)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, b.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func webArtifactConcurrentBrowsersConfig(tape string) string {
	cfg := replayConfig(tape)
	// The first replay row is the llm: slow its stream down.
	cfg = strings.Replace(cfg, fmt.Sprintf("config: {file: %q}", tape), fmt.Sprintf("config: {file: %q, delay_ms: 30}", tape), 1)
	return cfg + `
- id: tools
  plugin: tools-basic
- id: web
  plugin: web
  config: {addr: "127.0.0.1:0"}
- id: artifacts
  plugin: artifacts
  config: {open: false}
`
}

// webArtifactConcurrentBrowsersHistory is the newest session file.
func webArtifactConcurrentBrowsersHistory(a *app) string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var newest string
	var at time.Time
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.ModTime().Before(at) {
			newest, at = p, st.ModTime()
		}
	}
	return newest
}

// webArtifactConcurrentBrowsersNoticed is every press id the agent was
// told, in history order: landed mid-turn as a "job" entry, or as the
// "input" of the turn an idle wake starts.
func webArtifactConcurrentBrowsersNoticed(a *app) []string {
	entries, err := history.Read(webArtifactConcurrentBrowsersHistory(a))
	if err != nil {
		return nil
	}
	var out []string
	for _, e := range entries {
		if e.Kind != "job" && e.Kind != "input" {
			continue
		}
		text, _ := e.Data["text"].(string)
		for _, m := range webArtifactConcurrentBrowsersPressRe.FindAllStringSubmatch(text, -1) {
			out = append(out, m[1])
		}
	}
	return out
}

func TestWebArtifactConcurrentBrowsers(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, webArtifactConcurrentBrowsersConfig(webArtifactConcurrentBrowsersTape(t)))

	// The web row's random port, from /artifacts.
	a.typeText("/artifacts")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return webArtifactConcurrentBrowsersURLRe.MatchString(s) }, "/artifacts output with the web URL")
	port := webArtifactConcurrentBrowsersURLRe.FindStringSubmatch(a.text())[1]
	base := "http://127.0.0.1:" + port

	a.typeText("stream something long")
	a.key(uv.KeyEnter, 0)
	a.waitFor("streaming")

	// Publish a page into this session's store, the way tools.artifact
	// would: the replayed codemode never runs the real tool.
	sess := strings.TrimSuffix(filepath.Base(webArtifactConcurrentBrowsersHistory(a)), ".jsonl")
	if sess == "" || sess == "." {
		t.Fatal("no session file")
	}
	dir := filepath.Join(a.home, ".bough", "artifacts", sess)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "board.ui"), []byte("root = Card([title])\ntitle = TextContent(\"Pick\")\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	page := base + "/artifacts/" + sess + "/board"
	// The watcher's first sight of a page marks what is already logged
	// as seen (a resumed session is not news); tools.artifact does the
	// same at publish. Let one sweep (2 s) see the empty page first.
	time.Sleep(2500 * time.Millisecond)

	var mu sync.Mutex
	var fails []string
	fail := func(f string, args ...any) { mu.Lock(); fails = append(fails, fmt.Sprintf(f, args...)); mu.Unlock() }
	var wg sync.WaitGroup
	for c := 1; c <= 2; c++ {
		wg.Add(1)
		go func(c int) {
			defer wg.Done()
			cl := &http.Client{Timeout: 10 * time.Second}
			for i := 1; i <= webArtifactConcurrentBrowsersPresses; i++ {
				r, err := cl.Get(page)
				if err != nil {
					fail("client %d GET: %v", c, err)
					return
				}
				_, _ = io.Copy(io.Discard, r.Body)
				r.Body.Close()
				if r.StatusCode != 200 {
					fail("client %d GET page = %d", c, r.StatusCode)
				}
				body := fmt.Sprintf(`{"kind":"action","value":{"type":"continue_conversation","message":"c%d-p%d"}}`, c, i)
				r, err = cl.Post(page+"/answers", "application/json", strings.NewReader(body))
				if err != nil {
					fail("client %d POST: %v", c, err)
					return
				}
				b, _ := io.ReadAll(r.Body)
				r.Body.Close()
				if r.StatusCode != 200 {
					fail("client %d POST press %d = %d %s", c, i, r.StatusCode, b)
				}
			}
		}(c)
	}
	wg.Wait()
	for _, f := range fails {
		t.Error(f)
	}
	if n := a.doneCount(); n != 0 {
		t.Logf("turn already finished (%d) before the presses were in; not a mid-stream run", n)
	}

	// The log order the server serialised the presses into.
	r, err := http.Get(page + "/answers")
	if err != nil {
		t.Fatal(err)
	}
	var ans struct {
		Log []struct {
			Value struct{ Message string } `json:"value"`
		} `json:"log"`
	}
	_ = json.NewDecoder(r.Body).Decode(&ans)
	r.Body.Close()
	var logged []string
	for _, e := range ans.Log {
		logged = append(logged, e.Value.Message)
	}
	if len(logged) != 2*webArtifactConcurrentBrowsersPresses {
		t.Fatalf("answers log has %d presses, want %d: %v", len(logged), 2*webArtifactConcurrentBrowsersPresses, logged)
	}

	t.Run("TestWebArtifactConcurrentBrowsersEachPressWakesOnce", func(t *testing.T) {
		deadline := time.Now().Add(40 * time.Second)
		var got []string
		for time.Now().Before(deadline) {
			if got = webArtifactConcurrentBrowsersNoticed(a); len(got) >= len(logged) {
				break
			}
			time.Sleep(100 * time.Millisecond)
		}
		time.Sleep(5 * time.Second) // two more sweeps: a duplicate would show
		got = webArtifactConcurrentBrowsersNoticed(a)
		count := map[string]int{}
		for _, g := range got {
			count[g]++
		}
		for _, l := range logged {
			if count[l] != 1 {
				t.Errorf("press %s noticed %d times (want 1); notices %v", l, count[l], got)
			}
		}
		if len(got) != len(logged) {
			t.Errorf("%d notices for %d presses: %v", len(got), len(logged), got)
		}
		if strings.Join(got, ",") != strings.Join(logged, ",") {
			t.Errorf("notice order %v != answers log order %v", got, logged)
		}
	})

	a.check("after presses")

	t.Run("TestWebArtifactConcurrentBrowsersQuitFreesPort", func(t *testing.T) {
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
			t.Fatalf("process did not exit after two ctrl+c:\n%s", a.text())
		}
		ln, err := net.Listen("tcp", "127.0.0.1:"+port)
		if err != nil {
			t.Fatalf("port %s still taken after quit: %v", port, err)
		}
		ln.Close()
	})
}
