// Package memory is the "auto-memory" plugin: after every turn a small
// model reads what just happened and writes down what is worth
// remembering. Nothing else in bough records durable facts on its own —
// tools.graph.assert exists, but the agent has to think to call it, and
// mid-task it never does. This closes that gap without spending the
// agent's own model or its attention: the extraction runs after the
// turn is over, on the cheap "llm-small" service (see llm.Small), and
// its result is announced so the user can see what was remembered.
//
// Facts land in the graph plugin when it is mounted (the bi-temporal
// store, so a later contradiction supersedes rather than overwrites);
// without it they append to ~/.bough/memory.md, which is at least
// greppable.
package memory

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/graph"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// defaultMaxFacts caps one turn's harvest. A turn rarely establishes
// more than a couple of durable things, and an eager model will invent
// filler to reach whatever number it is given.
const defaultMaxFacts = 3

// digestBytes bounds what the small model reads: the tail of the turn,
// where the conclusion is.
const digestBytes = 6 * 1024

// Prompt is the extraction brief. It asks for pipe-separated triples
// rather than JSON: a small model gets one line per fact right far more
// often than it gets a nested object right.
const Prompt = `You read one finished turn of a coding session and write down ONLY what is worth remembering later.

Worth remembering: a decision and its reason, a constraint or preference the user stated, where something lives in the code, a non-obvious fact established by running something, a dead end proven not to work.

NOT worth remembering: what the user asked, what the agent did, anything a later reader could get by reading the code or git log, anything that is only true during this turn, pleasantries, plans.

Answer with at most %d lines, each exactly:
subject | relation | object | evidence

subject and object are kind:key, kinds: repo, file, package, tool, person, task, decision, service. Use the real paths and names from the turn.
relation is one of: relates, requires, replaces, blocked_by, decided, authored, implements, documents.
evidence is one sentence in plain words, quoting the number or path that makes it true.

If nothing in this turn is worth remembering, answer with exactly: NOTHING

The turn:
`

// History is the seam we read the turn from.
type History interface {
	Entries() []history.Entry
}

// Graph is the slice of the "graph" service we write through; absent,
// facts go to a file.
type Graph interface {
	AssertAs(author, src, rel, dst, evidence string) (graph.Edge, error)
}

// Memory is the plugin's state: the services, the cap, and the triples
// already written this session (a model asked the same question twice
// answers the same thing twice).
type Memory struct {
	llm      llm.LLM
	small    bool // a real llm-small row, not the agent's own model
	hist     History
	graph    Graph
	file     string
	maxFacts int
	emit     func(kind, text string)
	ctx      context.Context

	mu      sync.Mutex
	written map[string]bool
	busy    bool
	failed  bool // the extraction error has been reported once
}

// Fact is one extracted triple.
type Fact struct{ Src, Rel, Dst, Evidence string }

// Line renders a fact the way the memory file and the ui show it.
func (f Fact) Line() string {
	return fmt.Sprintf("%s %s %s — %s", f.Src, f.Rel, f.Dst, f.Evidence)
}

// key is the triple a fact is deduplicated on; the evidence wording may
// differ between turns without making it a new fact.
func (f Fact) key() string {
	return strings.ToLower(f.Src + "|" + f.Rel + "|" + f.Dst)
}

// ParseFacts reads the model's answer. Anything that is not a
// four-field line is dropped rather than guessed at: a small model that
// wandered off the format has nothing worth saving in that line.
func ParseFacts(reply string, max int) []Fact {
	if answer, ok := loop.StopAnswer(reply); ok {
		reply = answer
	}
	var out []Fact
	for _, line := range strings.Split(reply, "\n") {
		line = strings.TrimSpace(strings.TrimLeft(line, "-*0123456789. "))
		if line == "" || strings.EqualFold(line, "NOTHING") {
			continue
		}
		parts := strings.Split(line, "|")
		if len(parts) != 4 {
			continue
		}
		f := Fact{
			Src:      strings.TrimSpace(parts[0]),
			Rel:      strings.TrimSpace(parts[1]),
			Dst:      strings.TrimSpace(parts[2]),
			Evidence: strings.TrimSpace(parts[3]),
		}
		if f.Src == "" || f.Rel == "" || f.Dst == "" || f.Evidence == "" {
			continue
		}
		// A relation with spaces is fine; a subject without a kind is
		// not — it would create a junk entity.
		if !strings.Contains(f.Src, ":") || !strings.Contains(f.Dst, ":") {
			continue
		}
		if out = append(out, f); len(out) == max {
			break
		}
	}
	return out
}

// Digest is the turn as the extractor sees it: the user's message, what
// the agent finally said, and the files it wrote. Tool output is left
// out on purpose — it is the bulk of a turn and almost never the part
// worth remembering.
func Digest(entries []history.Entry) string {
	start := 0
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "input" {
			start = i
			break
		}
	}
	var b strings.Builder
	var files []string
	for _, e := range entries[start:] {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			// The typed line. The digest goes to a small model to
			// decide what is worth remembering, and an injected
			// skill's body is neither what the user asked nor
			// something worth paying for on every turn.
			b.WriteString("USER: " + history.Prompt(e) + "\n\n")
		case "assistant":
			if strings.TrimSpace(text) != "" {
				b.WriteString("AGENT: " + text + "\n\n")
			}
		case "code":
			b.WriteString("AGENT RAN:\n" + text + "\n\n")
		case "done":
			switch l := e.Data["files"].(type) {
			case []string:
				files = append(files, l...)
			case []any:
				for _, x := range l {
					if s, ok := x.(string); ok {
						files = append(files, s)
					}
				}
			}
		}
	}
	if len(files) > 0 {
		b.WriteString("FILES WRITTEN: " + strings.Join(files, ", ") + "\n")
	}
	s := b.String()
	if len(s) > digestBytes {
		s = "…\n" + s[len(s)-digestBytes:]
	}
	return s
}

// harvest runs one extraction. It is called off the turn's goroutine:
// a slow small model must never hold up the next prompt.
func (m *Memory) harvest() {
	m.mu.Lock()
	if m.busy {
		m.mu.Unlock()
		return // still working on the previous turn; skip this one
	}
	m.busy = true
	m.mu.Unlock()
	defer func() {
		m.mu.Lock()
		m.busy = false
		m.mu.Unlock()
	}()

	digest := Digest(m.hist.Entries())
	if strings.TrimSpace(digest) == "" {
		return
	}
	ctx, cancel := context.WithTimeout(m.ctx, 60*time.Second)
	defer cancel()
	reply, err := m.llm.Complete(ctx, fmt.Sprintf(Prompt, m.maxFacts), []llm.Message{{Role: "user", Content: digest}})
	if err != nil {
		// Never loud: a failed harvest costs nothing and the turn is
		// already over. It still says so ONCE, dimmed, so a broken
		// llm-small row is not silent forever — the comment promised
		// that before the flag existed, and an invalid key meant every
		// turn ended with the same provider error under it.
		m.mu.Lock()
		first := !m.failed
		m.failed = true
		m.mu.Unlock()
		if first {
			// One line: a provider's error carries its whole HTTP body,
			// and a receipt for a background job is not the place for it.
			m.emit("memory", "memory: extraction failed — "+firstLine(err.Error()))
		}
		return
	}
	facts := ParseFacts(reply, m.maxFacts)
	var saved []string
	for _, f := range facts {
		m.mu.Lock()
		dup := m.written[f.key()]
		m.written[f.key()] = true
		m.mu.Unlock()
		if dup {
			continue
		}
		if err := m.save(f); err != nil {
			m.emit("memory", "memory: "+err.Error())
			continue
		}
		saved = append(saved, f.Line())
	}
	if len(saved) == 0 {
		return // a turn that established nothing is not worth a row
	}
	head := fmt.Sprintf("remembered %d fact(s)", len(saved))
	if !m.small {
		head += " (no llm-small row: used the agent's model)"
	}
	m.emit("memory", head+"\n"+strings.Join(saved, "\n"))
}

// save writes one fact to the graph, or to the memory file without it.
func (m *Memory) save(f Fact) error {
	if m.graph != nil {
		// Signed "cheap": a small model's inference, never to be read
		// as something a source stated. The graph folds an unlisted
		// relation to "relates" and keeps the verb in the claim.
		if _, err := m.graph.AssertAs("cheap", f.Src, f.Rel, f.Dst, f.Evidence); err != nil {
			return fmt.Errorf("graph: %w", err)
		}
		return nil
	}
	fh, err := os.OpenFile(m.file, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer fh.Close()
	_, err = fmt.Fprintf(fh, "- %s\n", f.Line())
	return err
}

type plugin struct{}

func init() {
	kernel.Register("auto-memory", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "auto-memory" }
func (plugin) Inject() []string { return []string{"llm"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "max_facts" && k != "file" {
			return fmt.Errorf("auto-memory: unknown config key %q", k)
		}
	}
	max := defaultMaxFacts
	if v, ok := cfg["max_facts"]; ok {
		n, err := toInt(v)
		if err != nil || n < 1 {
			return fmt.Errorf("auto-memory: max_facts must be a positive integer, got %v", v)
		}
		max = n
	}
	l, small := llm.Small(kctx)
	if l == nil {
		return fmt.Errorf("auto-memory: no llm service")
	}
	h, err := kernel.Get[History](kctx, "history")
	if err != nil {
		return fmt.Errorf("auto-memory: needs the history service")
	}
	m := &Memory{llm: l, small: small, hist: h, maxFacts: max, written: map[string]bool{}}
	if g, err := kernel.Get[Graph](kctx, "graph"); err == nil {
		m.graph = g
	}
	if f, ok := cfg["file"].(string); ok && f != "" {
		m.file = f
	} else {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("auto-memory: home dir: %w", err)
		}
		m.file = filepath.Join(home, ".bough", "memory.md")
	}

	ctx, cancel := context.WithCancel(context.Background())
	m.ctx = ctx
	kctx.Effect(cancel)
	m.emit = func(kind, text string) {
		kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text})
	}
	// Every turn ends with a "done", whatever path it took.
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "done" {
			go m.harvest()
		}
	})
	kctx.Provide("auto-memory", m)
	return nil
}

// toInt reads a yaml int, float, or --set string.
func toInt(v any) (int, error) {
	switch n := v.(type) {
	case int:
		return n, nil
	case int64:
		return int(n), nil
	case float64:
		if n == float64(int(n)) {
			return int(n), nil
		}
	case string:
		var i int
		if _, err := fmt.Sscanf(n, "%d", &i); err == nil {
			return i, nil
		}
	}
	return 0, fmt.Errorf("not an integer: %v", v)
}

// firstLine is text up to its first newline, trimmed. A provider error
// arrives with its response body attached; the harvest receipt shows
// the sentence, not the JSON.
func firstLine(text string) string {
	if i := strings.IndexByte(text, '\n'); i >= 0 {
		text = text[:i]
	}
	return strings.TrimSpace(text)
}
