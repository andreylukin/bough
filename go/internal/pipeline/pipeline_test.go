package pipeline

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
)

// fakeSessions stands in for *serve.Supervisor. reply scripts each
// turn from its prompt; a reply of HANG never ends, ASK arms an ask,
// and SLOW:<text> ends after 150 ms.
type fakeSessions struct {
	mu      sync.Mutex
	reply   func(id, prompt string) string
	entries map[string][]history.Entry
	subs    map[string]map[int]chan serve.Event
	nextSub int
	live    map[string]bool
	asks    map[string]*serve.Ask
	created []serve.CreateOptions
	sent    map[string][]string
	killed  []string
}

func newFake(reply func(id, prompt string) string) *fakeSessions {
	return &fakeSessions{reply: reply, entries: map[string][]history.Entry{}, subs: map[string]map[int]chan serve.Event{},
		live: map[string]bool{}, asks: map[string]*serve.Ask{}, sent: map[string][]string{}}
}

func (f *fakeSessions) Create(opt serve.CreateOptions) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if opt.ID == "" {
		return "", errors.New("fake: no id")
	}
	f.created = append(f.created, opt)
	f.live[opt.ID] = true
	f.appendLocked(opt.ID, "meta", "")
	return opt.ID, nil
}

func (f *fakeSessions) appendLocked(id, kind, text string) {
	es := f.entries[id]
	f.entries[id] = append(es, history.Entry{Seq: int64(len(es) + 1), At: time.Now(), Kind: kind, Data: map[string]any{"text": text}})
}

func (f *fakeSessions) emitLocked(id, kind string) {
	for _, ch := range f.subs[id] {
		select {
		case ch <- serve.Event{Session: id, Kind: kind}:
		default:
		}
	}
}

func (f *fakeSessions) Send(id, text string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.sent[id] = append(f.sent[id], text)
	f.live[id] = true
	if strings.HasPrefix(text, "/model ") {
		return nil
	}
	f.appendLocked(id, "input", text)
	r := f.reply(id, text)
	switch {
	case r == "HANG":
	case r == "ASK":
		f.asks[id] = &serve.Ask{Text: "which?"}
	case strings.HasPrefix(r, "SLOW:"):
		go func() {
			time.Sleep(150 * time.Millisecond)
			f.mu.Lock()
			defer f.mu.Unlock()
			f.appendLocked(id, "assistant", strings.TrimPrefix(r, "SLOW:"))
			f.appendLocked(id, "done", "")
			f.emitLocked(id, "done")
		}()
	default:
		f.appendLocked(id, "assistant", r)
		f.appendLocked(id, "done", "")
		f.emitLocked(id, "done")
	}
	return nil
}

func (f *fakeSessions) Subscribe(id string) (<-chan serve.Event, func()) {
	f.mu.Lock()
	defer f.mu.Unlock()
	ch := make(chan serve.Event, 16)
	if f.subs[id] == nil {
		f.subs[id] = map[int]chan serve.Event{}
	}
	f.nextSub++
	k := f.nextSub
	f.subs[id][k] = ch
	return ch, func() {
		f.mu.Lock()
		defer f.mu.Unlock()
		delete(f.subs[id], k)
	}
}

func (f *fakeSessions) Entries(id string) ([]history.Entry, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]history.Entry(nil), f.entries[id]...), nil
}

func (f *fakeSessions) Live(id string) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.live[id]
}

func (f *fakeSessions) PendingAsk(id string) *serve.Ask {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.asks[id]
}

func (f *fakeSessions) Kill(id string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.killed = append(f.killed, id)
	f.live[id] = false
	f.emitLocked(id, "exit")
	return nil
}

func (f *fakeSessions) sentTo(id string) []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.sent[id]...)
}

func (f *fakeSessions) createdCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.created)
}

// writePipeline writes yml (and extra files) into a temp dir and loads it.
func writePipeline(t *testing.T, yml string, files map[string]string) (*Pipeline, error) {
	t.Helper()
	dir := t.TempDir()
	for name, body := range files {
		p := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	path := filepath.Join(dir, "pipeline.yml")
	if err := os.WriteFile(path, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	return Load(path)
}

const validYML = `
name: t
goal: make it green
start: coder
max_steps: 20
nodes:
  coder:
    type: agent
    prompt: "coder {{visit}}: {{goal}} / {{input}}"
    next: tests
    fail: coder
    max_visits: 3
  tests:
    type: check
    run: "exit 0"
    pass: done
    fail: coder
    max_visits: 3
`

func TestLoadValid(t *testing.T) {
	p, err := writePipeline(t, validYML, nil)
	if err != nil {
		t.Fatal(err)
	}
	if p.Nodes["coder"].Name != "coder" || p.Dir == "" {
		t.Errorf("node names/dir not filled: %+v", p)
	}
}

func TestValidateRejects(t *testing.T) {
	cases := []struct {
		name, yml, want string
		files           map[string]string
	}{
		{"bad start", strings.Replace(validYML, "start: coder", "start: nobody", 1), `start "nobody" is not a node`, nil},
		{"bad target", strings.Replace(validYML, "next: tests", "next: tset", 1), `next "tset" is not a node`, nil},
		{"missing max_visits", strings.Replace(validYML, "    max_visits: 3\n  tests:", "  tests:", 1), "coder: max_visits is required", nil},
		{"verdict on check", strings.Replace(validYML, `run: "exit 0"`, "run: \"exit 0\"\n    verdict: true", 1), "verdict is only for agent nodes", nil},
		{"check without pass", strings.Replace(validYML, "    pass: done\n", "", 1), "a check node needs pass and fail", nil},
		{"local without holdout", validYML + `  validator:
    type: agent
    holdout: [acceptance/*.md]
    verdict: true
    pass: done
    fail: coder
    max_visits: 2
`, "coder: a local agent node reads the whole host", map[string]string{"acceptance/a.md": "x"}},
		{"coach on check", validYML + "coaches:\n  - name: c\n    target: tests\n", `coach c: target "tests" is not an agent node`, nil},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := writePipeline(t, c.yml, c.files)
			if err == nil || !strings.Contains(err.Error(), c.want) {
				t.Errorf("err = %v, want %q", err, c.want)
			}
		})
	}
}

func TestVerdictOf(t *testing.T) {
	cases := []struct {
		reply    string
		pass, ok bool
	}{
		{"looks good\nVERDICT: PASS", true, true},
		{"broken\nVERDICT: FAIL\n", false, true},
		{"VERDICT: PASS  \n\n  \n", true, true},
		{"no verdict here", false, false},
		{"VERDICT: PASS\nbut then more", false, false},
		{"verdict: pass", false, false},
		{"", false, false},
	}
	for _, c := range cases {
		pass, ok := verdictOf(c.reply)
		if pass != c.pass || ok != c.ok {
			t.Errorf("verdictOf(%q) = %v,%v want %v,%v", c.reply, pass, ok, c.pass, c.ok)
		}
	}
}

func TestModelArgs(t *testing.T) {
	got := fmt.Sprint(modelArgs("llm-openai/gpt-6"))
	if got != "[--set llm.plugin=llm-openai --set llm.model=gpt-6]" {
		t.Errorf("modelArgs = %s", got)
	}
}
