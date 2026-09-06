package memtier

// The local memory: memoryd (go/memory/memoryd.py) keeps every chunk
// verbatim in SQLite, indexes it with BM25 plus a static embedding, and
// answers questions by having a small local model READ the top hits and
// return {seq, quote, answer}, which memoryd verifies against the chunk
// before anything is returned. This file is the client and what the
// plugin does with it: feed history into the index as it lands, write
// a ledger record per turn, hand the reasoner tools.recall(question),
// answer each new request from memory before the reasoner starts, and
// pick the hidden outputs a request is about.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

type memoryClient struct {
	url     string
	session string
	http    *http.Client
}

func newMemoryClient(url, session string) *memoryClient {
	return &memoryClient{url: strings.TrimRight(url, "/"), session: session, http: &http.Client{Timeout: 10 * time.Minute}}
}

func (c *memoryClient) post(ctx context.Context, path string, req map[string]any, out any) error {
	body, _ := json.Marshal(req)
	hr, err := http.NewRequestWithContext(ctx, http.MethodPost, c.url+path, bytes.NewReader(body))
	if err != nil {
		return err
	}
	hr.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(hr)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	var raw map[string]json.RawMessage
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		return fmt.Errorf("memoryd %s: %w", path, err)
	}
	if e, ok := raw["error"]; ok {
		var msg string
		_ = json.Unmarshal(e, &msg)
		return fmt.Errorf("memoryd %s: %s", path, msg)
	}
	b, _ := json.Marshal(raw)
	return json.Unmarshal(b, out)
}

// Index stores one chunk and returns its index line.
func (c *memoryClient) Index(ctx context.Context, seq int64, kind, text string) (string, error) {
	var out struct{ Line string }
	err := c.post(ctx, "/index", map[string]any{"session": c.session, "seq": seq, "kind": kind, "text": text}, &out)
	return out.Line, err
}

type hit struct {
	Session string
	Seq     int64
	Kind    string
	Line    string
}

// Search is the index alone: the chunks a query is about, this session
// only when session is true.
func (c *memoryClient) Search(ctx context.Context, query string, session bool, k int) ([]hit, error) {
	req := map[string]any{"query": query, "k": k}
	if session {
		req["session"] = c.session
	}
	var out struct{ Hits []hit }
	err := c.post(ctx, "/search", req, &out)
	return out.Hits, err
}

// Recalled is a verified answer, or Verified=false with nothing usable.
type Recalled struct {
	Answer   string
	Seq      int64
	Session  string
	Quote    string
	Verified bool
	Raw      string // the reader's unverified answer, for the receipt only
}

// Recall reads the top hits for a question; session=false searches
// every session the memory has, ledger records included.
func (c *memoryClient) Recall(ctx context.Context, question string, session bool) (Recalled, error) {
	req := map[string]any{"question": question}
	if session {
		req["session"] = c.session
	}
	var out Recalled
	err := c.post(ctx, "/recall", req, &out)
	return out, err
}

type fact struct {
	Seq     int64
	Session string
	Quote   string
	Fact    string
}

// Note is the memory's verified facts for a new request.
func (c *memoryClient) Note(ctx context.Context, request string) ([]fact, error) {
	var out struct{ Facts []fact }
	err := c.post(ctx, "/note", map[string]any{"request": request, "session": c.session}, &out)
	return out.Facts, err
}

// Consolidate writes ledger records for one turn's chunks.
func (c *memoryClient) Consolidate(ctx context.Context, from, to int64) (int, error) {
	var out struct{ Records int }
	err := c.post(ctx, "/consolidate", map[string]any{"session": c.session, "from_seq": from, "to_seq": to}, &out)
	return out.Records, err
}

// sessionName is the memoryd session for a history file: its base
// name, so a resumed session finds its own chunks.
func sessionName(path string) string {
	if path == "" {
		return "default"
	}
	return strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
}

// feeder streams history entries into the index in seq order, one at
// a time, off the turn's goroutine, and consolidates each finished
// turn into the ledger.
type feeder struct {
	c     *memoryClient
	hist  interface{ Entries() []history.Entry }
	ctx   context.Context
	fail  func(error)
	lines func(seq int64, line string) // the placeholder line for a chunk, as memoryd wrote it

	mu        sync.Mutex
	seq       int64 // high-water: every entry <= seq is indexed
	busy      bool
	again     bool
	pendingCn []int64 // "done" seqs whose turns await consolidation
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

// TurnDone marks the turn ending at seq for consolidation once fed.
func (f *feeder) TurnDone(seq int64) {
	f.mu.Lock()
	f.pendingCn = append(f.pendingCn, seq)
	f.mu.Unlock()
	f.Kick()
}

func (f *feeder) run() {
	for {
		f.mu.Lock()
		f.again = false
		hw := f.seq
		f.mu.Unlock()
		lastCode := ""
		for _, e := range f.hist.Entries() {
			if e.Kind == "code" {
				lastCode, _ = e.Data["text"].(string)
			}
			if e.Seq <= hw {
				continue
			}
			if kind, text := feedText(e); text != "" {
				// A recall's own output is memory talking about memory;
				// stored for the record, never used as evidence.
				if kind == "tool output" && strings.Contains(lastCode, "tools.recall(") {
					kind = "recall"
				}
				line, err := f.c.Index(f.ctx, e.Seq, kind, text)
				if err != nil {
					f.fail(err)
					f.mu.Lock()
					f.busy = false
					f.mu.Unlock()
					return
				}
				if e.Kind == "result" && f.lines != nil {
					f.lines(e.Seq, line)
				}
			}
			hw = e.Seq
			f.mu.Lock()
			f.seq = hw
			f.mu.Unlock()
		}
		// Turns whose entries are all in: one ledger pass each.
		f.mu.Lock()
		var ready []int64
		var later []int64
		for _, s := range f.pendingCn {
			if s <= f.seq {
				ready = append(ready, s)
			} else {
				later = append(later, s)
			}
		}
		f.pendingCn = later
		f.mu.Unlock()
		for _, s := range ready {
			// The turn is everything from its input up to the done.
			start := int64(1)
			for _, e := range f.hist.Entries() {
				if e.Kind == "input" && e.Seq <= s {
					start = e.Seq
				}
			}
			if _, err := f.c.Consolidate(f.ctx, start, s); err != nil {
				f.fail(err)
			}
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

// Wait blocks until the feeder is idle or the deadline passes.
func (f *feeder) Wait(d time.Duration) {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		f.mu.Lock()
		idle := !f.busy && !f.again
		f.mu.Unlock()
		if idle {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
}

var spillRe = regexp.MustCompile(`\[full output saved to (\S+) — \d+ lines;`)

// spillMax caps what one spilled output feeds the index.
const spillMax = 400_000

// feedText is what one history entry contributes: its kind for the
// index and its text, spilled outputs read back in full.
func feedText(e history.Entry) (string, string) {
	text, _ := e.Data["text"].(string)
	switch e.Kind {
	case "input":
		return "user", text
	case "assistant":
		return "agent", text
	case "code":
		return "code", text
	case "result":
		if m := spillRe.FindStringSubmatch(text); m != nil {
			if full, err := os.ReadFile(m[1]); err == nil && len(full) <= spillMax {
				text = string(full)
			}
		}
		return "tool output", text
	case "job":
		return "background job", text
	}
	return "", ""
}

// note answers a new request from memory, once per turn: the verified
// facts, each with the seq it came from so the reasoner can focus it.
// "" when the memory has nothing, so the projection is untouched.
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
	facts, err := t.mem.Note(ctx, prompt)
	if err != nil {
		t.reportOnce(err)
	}
	var b strings.Builder
	for _, f := range facts {
		fmt.Fprintf(&b, "- %s (from #%d: %q)\n", f.Fact, f.Seq, f.Quote)
	}
	a := strings.TrimSpace(b.String())
	t.mu.Lock()
	t.notes[inputSeq] = a
	t.mu.Unlock()
	if a != "" && t.emit != nil {
		t.emit("memory", "memory: "+a)
	}
	return a
}

// recall is tools.recall(question) -> string: a verified value with its
// source, or a plain statement that memory has nothing verified.
func (t *Tier) recall(question string) (string, error) {
	if t.mem == nil {
		return "", fmt.Errorf("recall: no local memory configured (memory-tier memory_url)")
	}
	ctx, cancel := context.WithTimeout(t.ctx, 2*time.Minute)
	defer cancel()
	r, err := t.mem.Recall(ctx, question, false)
	if err != nil {
		return "", err
	}
	if !r.Verified {
		if r.Raw != "" {
			return "not in memory (the reader guessed " + strconv.Quote(r.Raw) + " but no stored output contains it; do not use it)", nil
		}
		return "not in memory", nil
	}
	where := "#" + strconv.FormatInt(r.Seq, 10)
	if r.Session != t.mem.session {
		where = "session " + r.Session + " " + where
	}
	return fmt.Sprintf("%s\n(verified in %s: %q)", r.Answer, where, r.Quote), nil
}
