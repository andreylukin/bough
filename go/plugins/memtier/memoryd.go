package memtier

// The local memory model. memoryd (~/.bough/memory/memoryd.py) holds a
// hybrid model's recurrent state per session on mlx-lm: everything the
// agent sees is appended to it once, in order, and questions fork the
// state to answer. This file is the client and the three things the
// plugin does with it: feed the history into it as it grows, hand the
// reasoner tools.recall(question), and answer each new request from
// memory before the reasoner starts.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// memoryClient talks to one memoryd.
type memoryClient struct {
	url     string
	session string
	http    *http.Client
}

func newMemoryClient(url, session string) *memoryClient {
	return &memoryClient{url: strings.TrimRight(url, "/"), session: session, http: &http.Client{Timeout: 10 * time.Minute}}
}

func (c *memoryClient) post(ctx context.Context, path string, req map[string]any) (map[string]any, error) {
	req["session"] = c.session
	body, _ := json.Marshal(req)
	hr, err := http.NewRequestWithContext(ctx, http.MethodPost, c.url+path, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	hr.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(hr)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	var out map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return nil, fmt.Errorf("memoryd %s: %w", path, err)
	}
	if e, ok := out["error"].(string); ok && e != "" {
		return nil, fmt.Errorf("memoryd %s: %s", path, e)
	}
	return out, nil
}

// Ingest appends text to the session state under history seq.
func (c *memoryClient) Ingest(ctx context.Context, seq int64, text string) error {
	_, err := c.post(ctx, "/ingest", map[string]any{"seq": seq, "text": text})
	return err
}

// Ask answers a question from the state.
func (c *memoryClient) Ask(ctx context.Context, question string, maxTokens int) (string, error) {
	out, err := c.post(ctx, "/ask", map[string]any{"question": question, "max_tokens": maxTokens})
	if err != nil {
		return "", err
	}
	a, _ := out["answer"].(string)
	return a, nil
}

// Save writes the state to disk. Load reads it back and returns the
// history seq it had reached (0 when there was nothing).
func (c *memoryClient) Save(ctx context.Context) error {
	_, err := c.post(ctx, "/save", map[string]any{})
	return err
}

func (c *memoryClient) Load(ctx context.Context) (int64, error) {
	out, err := c.post(ctx, "/load", map[string]any{})
	if err != nil {
		return 0, err
	}
	seq, _ := out["seq"].(float64)
	return int64(seq), nil
}

// Complete makes the memory usable wherever an llm.LLM is: the
// navigator's index and pick prompts go to it as one question. The
// state already holds every output, so the question is answered from
// memory plus whatever the prompt carries.
func (c *memoryClient) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	var b strings.Builder
	b.WriteString(system)
	for _, m := range messages {
		b.WriteString("\n\n")
		b.WriteString(m.Content)
	}
	return c.Ask(ctx, b.String(), 400)
}

// sessionName is the memoryd session for a history file: its base
// name, so a resumed session finds its own state.
func sessionName(path string) string {
	if path == "" {
		return "default"
	}
	return strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
}

// feeder streams history entries into the memory in seq order, one at
// a time, off the turn's goroutine.
type feeder struct {
	c    *memoryClient
	hist interface{ Entries() []history.Entry }
	ctx  context.Context
	fail func(error)

	mu    sync.Mutex
	seq   int64 // high-water: every entry <= seq is in the memory
	busy  bool
	again bool
}

// Kick feeds whatever is new; a call during a run schedules one more.
func (f *feeder) Kick() {
	f.mu.Lock()
	if f.busy {
		f.again = true
		f.mu.Unlock()
		return
	}
	f.busy = true
	f.mu.Unlock()
	go f.run()
}

func (f *feeder) run() {
	for {
		f.mu.Lock()
		f.again = false
		hw := f.seq
		f.mu.Unlock()
		for _, e := range f.hist.Entries() {
			if e.Seq <= hw {
				continue
			}
			text := feedText(e)
			if text != "" {
				if err := f.c.Ingest(f.ctx, e.Seq, text); err != nil {
					f.fail(err)
					f.mu.Lock()
					f.busy = false
					f.mu.Unlock()
					return
				}
			}
			hw = e.Seq
			f.mu.Lock()
			f.seq = hw
			f.mu.Unlock()
		}
		f.mu.Lock()
		if !f.again {
			f.busy = false
			f.mu.Unlock()
			return
		}
		f.mu.Unlock()
	}
}

// Wait blocks until the feeder is idle or the deadline passes, so a
// save lands after the turn's own entries.
func (f *feeder) Wait(d time.Duration) {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		f.mu.Lock()
		idle := !f.busy && !f.again
		f.mu.Unlock()
		if idle {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
}

var spillRe = regexp.MustCompile(`\[full output saved to (\S+) — \d+ lines;`)

// spillMax caps what one spilled output feeds: past this the model's
// state is better served by the head and tail the agent saw.
const spillMax = 400_000

// feedText is what one history entry contributes to the memory: the
// same kinds the model is shown, headed by seq and kind, so an answer
// can cite the seq and the reasoner can focus it.
func feedText(e history.Entry) string {
	text, _ := e.Data["text"].(string)
	switch e.Kind {
	case "input":
		return fmt.Sprintf("\n[#%d user]\n%s\n", e.Seq, text)
	case "assistant":
		return fmt.Sprintf("\n[#%d agent]\n%s\n", e.Seq, text)
	case "result":
		// A capped output names its spill file; the memory reads the
		// whole thing, which is the point of having it.
		if m := spillRe.FindStringSubmatch(text); m != nil {
			if full, err := os.ReadFile(m[1]); err == nil && len(full) <= spillMax {
				text = string(full)
			}
		}
		return fmt.Sprintf("\n[#%d tool output]\n%s\n", e.Seq, text)
	case "job":
		return fmt.Sprintf("\n[#%d background job]\n%s\n", e.Seq, text)
	}
	return ""
}

const notePrompt = `The user's new request to the agent is below. From the session so far, list the facts the agent will need for it: exact values, file paths, commands already run and what they returned, errors, decisions. Cite the [#SEQ] of the output each fact came from. Be brief; at most eight lines. If nothing in the session bears on it, reply exactly: nothing relevant.

Request: `

// note answers a new request from memory, once per turn. "" when the
// memory has nothing, so the projection is untouched.
func (t *Tier) note(inputSeq int64, prompt string) string {
	if t.mem == nil || inputSeq == 0 || strings.TrimSpace(prompt) == "" {
		return ""
	}
	t.mu.Lock()
	n, done := t.notes[inputSeq]
	t.mu.Unlock()
	if done {
		return n
	}
	ctx, cancel := context.WithTimeout(t.ctx, t.pickTimeout)
	defer cancel()
	a, err := t.mem.Ask(ctx, notePrompt+prompt, 300)
	if err != nil {
		t.reportOnce(err)
		a = ""
	}
	a = strings.TrimSpace(a)
	if strings.EqualFold(strings.TrimRight(a, "."), "nothing relevant") || strings.EqualFold(strings.TrimRight(a, "."), "not in memory") {
		a = ""
	}
	t.mu.Lock()
	t.notes[inputSeq] = a
	t.mu.Unlock()
	if a != "" && t.emit != nil {
		t.emit("memory", "memory: "+a)
	}
	return a
}

// recall is tools.recall(question) -> string: the reasoner asks the
// memory directly.
func (t *Tier) recall(question string) (string, error) {
	if t.mem == nil {
		return "", fmt.Errorf("recall: no local memory configured (memory-tier memory_url)")
	}
	ctx, cancel := context.WithTimeout(t.ctx, 2*time.Minute)
	defer cancel()
	return t.mem.Ask(ctx, question, 400)
}
