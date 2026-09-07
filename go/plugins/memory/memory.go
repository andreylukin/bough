// Package memory is the "auto-memory" plugin: after every turn a small
// model reads what just happened and writes down what is worth
// remembering. Nothing else in bough records durable facts on its own —
// tools.graph.assert exists, but the agent has to think to call it, and
// mid-task it never does. This closes that gap without spending the
// agent's own model or its attention: the extraction runs after the
// turn is over, on the cheap "llm-small" service (see llm.Small), and
// its result is announced so the user can see what was remembered.
// Facts are plain sentences under four kinds (preference, decision,
// location, dead-end), not graph triples: a small model writes a
// sentence right far more often than a schema, and a sentence is what
// the agent can act on when it reads it back. The row also owns the
// prompt rule that tells the agent when to search memory at all.
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
	"regexp"
	"slices"
	"strconv"
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
const digestBytes = 12 * 1024

// resultHead is how much of one tool output the digest carries: enough
// for the value a fact turns on (a count, a path, an error line), not
// the dump. Facts are verified against the full output afterwards.
const resultHead = 600

// Prompt is the extraction brief. It asks for one plain sentence per
// fact, pipe-separated from its kind and evidence, rather than graph
// triples or JSON: a small model gets a sentence right far more often
// than it gets a schema right, and a sentence is what a later reader
// (the agent, or the user in the transcript) can actually use.
const Prompt = `You read one finished turn of a coding session and write down ONLY what is worth remembering in a LATER session.

Worth remembering: a preference or constraint the user stated and why, a decision and its reason, where something non-obvious lives in the code, a dead end proven not to work and why.

NOT worth remembering: what the user asked, what the agent did, anything a later reader could get by reading the code or git log, anything that is only true during this turn, test results, pleasantries, plans.

Answer with at most %d lines, each exactly:
kind | fact | evidence

kind is one of: preference, decision, location, dead-end.
fact is ONE plain sentence, specific enough to be useful with no other context: name the real paths, flags, names and the reason. Write "the user" for the user.
evidence is #N followed by a colon and a short verbatim quote from the entry marked [#N] in the turn that makes the fact true, e.g. #12: never use git stash here

If nothing in this turn is worth remembering, answer with exactly: NOTHING

The turn:
`

// Kinds is the closed set of fact kinds; a line with any other kind is
// the model wandering and is dropped.
var Kinds = []string{"preference", "decision", "location", "dead-end"}

// Rule is the prompt section that tells the agent what memory is and
// when to consult it. It is present from the first turn, whether or
// not the graph has anything about this workspace yet: without it the
// agent only ever searched memory when told to.
const Rule = `Memory — facts from earlier sessions (the user's preferences, decisions and their reasons, where things live, dead ends) are kept across sessions; the ones about this workspace are listed under "## memory" when there are any. %s Search it BEFORE answering about a past decision or an earlier session, when the user refers to something as already settled ("as before", "like last time", "we decided", "you know"), and when starting in an unfamiliar part of the code. After every turn a small model records on its own what was worth remembering, so you need not; %s`

// RuleGraph and RuleFile are the halves of Rule that name where memory
// is: the graph verbs when the graph row is mounted, the file otherwise.
const (
	RuleGraph     = `tools.graph.search(q) finds more, including facts from other workspaces.`
	RuleGraphSave = `call tools.graph.assert only when the user asks you to remember something, quoting their words as the evidence.`
	RuleFile      = `tools.bash("grep -i <word> %s") finds more.`
	RuleFileSave  = `when the user asks you to remember something, append one line to that file.`
)

// History is the seam we read the turn from. Path names the session
// file, which is the session half of an evidence reference.
type History interface {
	Entries() []history.Entry
	Path() string
}

// Codemode is the optional seam that gives the model tools.evidence.
type Codemode interface {
	RegisterTool(name string, fn any)
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
	session  string // the session file's base name, cited in evidence
	graph    Graph
	subject  string // the entity facts hang off: repo:<key>, else the session
	file     string
	maxFacts int
	emit     func(kind, text string)
	ctx      context.Context

	mu      sync.Mutex
	written map[string]bool
	busy    bool
	failed  bool // the extraction error has been reported once
}

// Fact is one extracted sentence with its kind. Seq and Quote are the
// evidence taken apart: the history entry it cites and the words it
// quotes from it.
type Fact struct {
	Kind, Text, Evidence string
	Seq                  int64
	Quote                string
}

// Line renders a fact the way the ui shows it: the sentence, under
// its kind. The evidence is in the store for tools.evidence, not in
// the receipt.
func (f Fact) Line() string {
	return f.Kind + ": " + f.Text
}

// key is what a fact is deduplicated on; the evidence wording may
// differ between turns without making it a new fact.
func (f Fact) key() string {
	return f.Kind + "|" + squash(f.Text)
}

// slug is the fact's entity key in the graph: its kind and the first
// words of the sentence, so two facts of one kind are two edges (the
// store folds an identical src/rel/dst into one) and a key stays
// readable in `bough graph neighbors`.
func (f Fact) slug() string {
	words := strings.Fields(strings.ToLower(f.Text))
	if len(words) > 8 {
		words = words[:8]
	}
	s := strings.Join(words, "-")
	s = strings.Map(func(r rune) rune {
		if r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-' || r == '.' || r == '/' || r == '_' {
			return r
		}
		return -1
	}, s)
	return f.Kind + "/" + strings.Trim(s, "-")
}

var evidenceRe = regexp.MustCompile(`^#(\d+)\s*[:\-–—]\s*(.+)$`)

// Verify checks a fact's quote against the turn: it must occur, case
// and whitespace aside, in the entry it cites, else in some entry of
// the turn (the seq is corrected). A quote found nowhere is the model
// inventing, and the fact is dropped. Facts with no #seq evidence are
// kept as they were, unverified.
func Verify(f Fact, turn []history.Entry) (Fact, bool) {
	if f.Seq == 0 || f.Quote == "" {
		return f, true
	}
	needle := squash(f.Quote)
	var fallback int64
	for _, e := range turn {
		text, _ := e.Data["text"].(string)
		if !strings.Contains(squash(text), needle) {
			continue
		}
		if e.Seq == f.Seq {
			return f, true
		}
		if fallback == 0 {
			fallback = e.Seq
		}
	}
	if fallback == 0 {
		return f, false
	}
	f.Seq = fallback
	f.Evidence = fmt.Sprintf("#%d: %s", fallback, f.Quote)
	return f, true
}

func squash(s string) string {
	return strings.ToLower(strings.Join(strings.Fields(s), " "))
}

// ParseFacts reads the model's answer. Anything that is not a
// three-field line with a known kind is dropped rather than guessed
// at: a small model that wandered off the format has nothing worth
// saving in that line.
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
		if len(parts) != 3 {
			continue
		}
		f := Fact{
			Kind:     strings.ToLower(strings.TrimSpace(parts[0])),
			Text:     strings.TrimSpace(parts[1]),
			Evidence: strings.TrimSpace(parts[2]),
		}
		f.Kind = strings.ReplaceAll(strings.ReplaceAll(f.Kind, " ", "-"), "_", "-")
		if f.Text == "" || f.Evidence == "" || !slices.Contains(Kinds, f.Kind) {
			continue
		}
		if m := evidenceRe.FindStringSubmatch(f.Evidence); m != nil {
			f.Seq, _ = strconv.ParseInt(m[1], 10, 64)
			f.Quote = strings.TrimSpace(m[2])
		}
		if out = append(out, f); len(out) == max {
			break
		}
	}
	return out
}

// Turn is the entries of the last turn: from its input to the end.
func Turn(entries []history.Entry) []history.Entry {
	start := 0
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "input" {
			start = i
			break
		}
	}
	return entries[start:]
}

// Digest is the turn as the extractor sees it: the user's message, what
// the agent said and ran, the head of each tool output, and the files
// it wrote. Every entry is marked [#seq] so a fact can cite the entry
// its quote came from; Verify then holds it to that.
func Digest(entries []history.Entry) string {
	var b strings.Builder
	var files []string
	for _, e := range Turn(entries) {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			// The typed line. The digest goes to a small model to
			// decide what is worth remembering, and an injected
			// skill's body is neither what the user asked nor
			// something worth paying for on every turn.
			fmt.Fprintf(&b, "[#%d] USER: %s\n\n", e.Seq, history.Prompt(e))
		case "assistant":
			if strings.TrimSpace(text) != "" {
				fmt.Fprintf(&b, "[#%d] AGENT: %s\n\n", e.Seq, text)
			}
		case "code":
			fmt.Fprintf(&b, "[#%d] AGENT RAN:\n%s\n\n", e.Seq, text)
		case "result":
			head := text
			if len(head) > resultHead {
				head = head[:resultHead] + "…"
			}
			fmt.Fprintf(&b, "[#%d] OUTPUT:\n%s\n\n", e.Seq, head)
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

	entries := m.hist.Entries()
	digest := Digest(entries)
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
	kernel.Logf("auto-memory: reply:\n%s\n", strings.TrimSpace(reply))
	facts := ParseFacts(reply, m.maxFacts)
	turn := Turn(entries)
	var saved []string
	dropped := 0
	for _, f := range facts {
		// The quote must be in the turn, or the fact is the model's
		// invention. A verified fact's evidence names the session and
		// entry, so a later reader can open it: tools.evidence(ref).
		f, ok := Verify(f, turn)
		if !ok {
			kernel.Logf("auto-memory: dropped, quote not in turn: %s\n", f.Line())
			dropped++
			continue
		}
		if f.Seq != 0 {
			f.Evidence = fmt.Sprintf("%s#%d: %s", m.session, f.Seq, f.Quote)
		}
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
	// The receipt leads with the facts themselves; the header line is
	// what a collapsed row shows, so it is the first fact, not a count.
	var tail []string
	if dropped > 0 {
		tail = append(tail, fmt.Sprintf("dropped %d whose quote was not in the turn", dropped))
	}
	if !m.small {
		tail = append(tail, "no llm-small row: used the agent's model")
	}
	text := strings.Join(saved, "\n")
	if len(tail) > 0 {
		text += "\n(" + strings.Join(tail, "; ") + ")"
	}
	m.emit("memory", text)
}

// save writes one fact to the graph, or to the memory file without it.
func (m *Memory) save(f Fact) error {
	if m.graph != nil {
		// An edge from the workspace to a memory entity, whose claim
		// is the sentence: the graph's prompt section prints claims,
		// so the fact comes back in later sessions as it was written.
		// Signed "cheap": a small model's inference, never to be read
		// as something a source stated.
		claim := f.Line() + " — " + f.Evidence
		if _, err := m.graph.AssertAs("cheap", m.subject, "relates", "memory:"+f.slug(), claim); err != nil {
			return fmt.Errorf("graph: %w", err)
		}
		return nil
	}
	fh, err := os.OpenFile(m.file, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer fh.Close()
	_, err = fmt.Fprintf(fh, "- %s — %s\n", f.Line(), f.Evidence)
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
	m := &Memory{llm: l, small: small, hist: h, session: sessionName(h.Path()), maxFacts: max, written: map[string]bool{}}
	if g, err := kernel.Get[Graph](kctx, "graph"); err == nil {
		m.graph = g
	}
	// Facts hang off the workspace's repo, which is what the graph's
	// prompt section seeds from; outside a checkout, off the session.
	cwd, _ := os.Getwd()
	if ws := graph.Workspace(cwd); ws.Repo != "" {
		m.subject = "repo:" + ws.Repo
	} else {
		m.subject = "session:" + m.session
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
	// tools.evidence(ref): the verbatim entry an edge's evidence names,
	// from this session or any other on disk. Optional seam: headless
	// runs without codemode have no tools.
	if cm, err := kernel.Get[Codemode](kctx, "codemode"); err == nil {
		cm.RegisterTool("evidence", m.evidence)
		if d, ok := cm.(interface{ Describe(name, line string) }); ok {
			d.Describe("evidence", `tools.evidence("session#seq") -> string: the full text of the history entry a graph edge's evidence cites, verbatim; "#seq" alone means this session.`)
		}
	}
	// The rule that says what memory is and when to look in it.
	if s, err := kernel.Get[sections](kctx, "prompt-sections"); err == nil {
		s.Set("memory-rule", m.rule())
		kctx.Effect(func() { s.Set("memory-rule", "") })
	}
	kctx.Provide("auto-memory", m)
	return nil
}

// sections is the loop's prompt-sections registry.
type sections interface{ Set(name, text string) }

// rule is the prompt section for this mount: the graph's verbs when
// the graph row is present, the file otherwise.
func (m *Memory) rule() string {
	if m.graph != nil {
		return fmt.Sprintf(Rule, RuleGraph, RuleGraphSave)
	}
	return fmt.Sprintf(Rule, fmt.Sprintf(RuleFile, m.file), RuleFileSave)
}

// sessionName is the session half of an evidence reference: the
// history file's base name.
func sessionName(path string) string {
	if path == "" {
		return "session"
	}
	return strings.TrimSuffix(filepath.Base(path), filepath.Ext(path))
}

// evidence resolves "session#seq" (or "#seq") to the entry's text.
func (m *Memory) evidence(ref string) (string, error) {
	sess, seqs, ok := strings.Cut(strings.TrimSpace(ref), "#")
	if !ok {
		return "", fmt.Errorf("evidence: want session#seq, got %q", ref)
	}
	seq, err := strconv.ParseInt(strings.TrimSpace(strings.SplitN(seqs, ":", 2)[0]), 10, 64)
	if err != nil {
		return "", fmt.Errorf("evidence: bad seq in %q", ref)
	}
	sess = strings.TrimSpace(sess)
	var entries []history.Entry
	if sess == "" || sess == m.session {
		entries = m.hist.Entries()
	} else {
		if strings.ContainsAny(sess, "/\\") {
			return "", fmt.Errorf("evidence: bad session %q", sess)
		}
		entries, err = history.Read(filepath.Join(filepath.Dir(m.hist.Path()), sess+".jsonl"))
		if err != nil {
			return "", fmt.Errorf("evidence: session %s: %w", sess, err)
		}
	}
	for _, e := range entries {
		if e.Seq == seq {
			text, _ := e.Data["text"].(string)
			return fmt.Sprintf("[%s#%d %s]\n%s", sessionOf(sess, m.session), seq, e.Kind, text), nil
		}
	}
	return "", fmt.Errorf("evidence: no entry #%d in session %s", seq, sessionOf(sess, m.session))
}

func sessionOf(given, own string) string {
	if given == "" {
		return own
	}
	return given
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
