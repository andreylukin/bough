// Package memtier is the "memory-tier" plugin: a two-tier memory for
// long sessions. The history on disk is the memory; the model is shown
// a projection of it in which old tool outputs collapse to one-line
// placeholders and come back in full when the model declares it needs
// them. A small navigator model (the llm-small row) writes the index
// line for each output and, at the start of a turn, picks the outputs
// the prompt is about. The placeholder itself carries the protocol, so
// no system-prompt section is needed.
//
// The design follows three results. Declarative Attention (Ho et al.,
// arXiv 2609.02737) has the model declare which context segments it
// attends to; here the segments are tool results and the declaration
// is a <focus seq=N> tag the projector honours on the next step, so
// nothing is ever deleted and any projection is reversible. PACE (Wei
// et al., ACL 2026) scores each history chunk for the next step and
// shows it at a granularity to match. Index lines are written off the
// turn; only the per-turn pick is on the turn's path.
//
// A local memory model behind this projector was built and measured in
// September 2026 and removed; docs/memory-experiment.md has the numbers.
//
// Nothing here is compaction: the history log keeps every byte and
// /export, resume and `bough log` are unchanged.
package memtier

import (
	"context"
	"fmt"
	"maps"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

const (
	// defaultBudget is the projection size, in characters, above
	// which old tool outputs start collapsing. Under it the projection
	// is exactly the loop's own; a short session is never touched.
	defaultBudget = 100_000
	// defaultKeepWhole recent tool outputs always stay in full: they
	// are what the model is working from right now.
	defaultKeepWhole = 6
	// defaultTopK is how many hidden outputs the navigator may bring
	// back for a turn.
	defaultTopK = 6
	// defaultPickTimeout bounds the navigator's per-turn pick, which
	// runs on the turn's path; past it the turn goes on without it.
	defaultPickTimeout = 10 * time.Second
	// inlineBelow is the output size under which the first line IS the
	// index line: no model call is worth it.
	inlineBelow = 800
	// indexBatch is how many outputs one navigator call summarises.
	indexBatch = 8
	// indexInput caps the text of one output handed to the navigator.
	indexInput = 6_000
)

var focusRe = regexp.MustCompile(`<focus\s+seq=["']?([0-9][0-9,\s]*)["']?\s*/?>`)

// ParseFocus is the seqs declared by <focus seq=…> tags in text.
func ParseFocus(text string) []int64 {
	var out []int64
	for _, m := range focusRe.FindAllStringSubmatch(text, -1) {
		for f := range strings.FieldsFuncSeq(m[1], func(r rune) bool { return r == ',' || r == ' ' || r == '\n' || r == '\t' }) {
			if n, err := strconv.ParseInt(f, 10, 64); err == nil {
				out = append(out, n)
			}
		}
	}
	slices.Sort(out)
	return slices.Compact(out)
}

// Tier is a projector; it satisfies loop.Projection.
type Tier struct {
	// nav resolves the navigator at each use, never at mount: this row
	// mounts before the loop, and a service looked up and missed during
	// Apply would remount the row (and the loop with it, since it
	// re-provides "projection") the moment that service appeared.
	nav         func() llm.LLM
	budget      int
	keepWhole   int
	topK        int
	pickTimeout time.Duration
	emit        func(kind, text string)
	ctx         context.Context

	mu     sync.Mutex
	index  map[int64]string  // seq -> index line (navigator-written)
	picked map[int64][]int64 // input seq -> seqs the navigator chose
	busy   bool
	failed bool
}

// New returns a projector with the given navigator source (nil, or
// one returning nil, = no model: first lines and recency only).
func New(nav func() llm.LLM) *Tier {
	return &Tier{
		nav: nav, budget: defaultBudget, keepWhole: defaultKeepWhole, topK: defaultTopK,
		pickTimeout: defaultPickTimeout, ctx: context.Background(),
		index: map[int64]string{}, picked: map[int64][]int64{},
	}
}

// Project is the loop's projection: DefaultProject over the entries,
// with old tool outputs replaced by their placeholder when the whole
// would exceed the budget.
func (t *Tier) Project(entries []history.Entry) []llm.Message {
	total := 0
	var results []int // indices of result entries
	var inputSeq int64
	var reply strings.Builder // assistant text of the current turn
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		total += len(text)
		switch e.Kind {
		case "result":
			results = append(results, i)
		case "input":
			inputSeq = e.Seq
			reply.Reset()
		case "assistant":
			reply.WriteString(text)
			reply.WriteByte('\n')
		}
	}
	prompt := ""
	for _, e := range entries {
		if e.Seq == inputSeq {
			prompt, _ = e.Data["text"].(string)
		}
	}
	if total <= t.budget || len(results) <= t.keepWhole {
		return loop.DefaultProject(entries)
	}
	keep := map[int64]bool{}
	for _, i := range results[len(results)-t.keepWhole:] {
		keep[entries[i].Seq] = true
	}
	for _, s := range ParseFocus(reply.String()) {
		keep[s] = true
	}
	// Everything older is a candidate; the navigator picks from them
	// once per turn, and the pick runs on the turn's path with a
	// deadline so the model is never held long.
	var cands []history.Entry
	for _, i := range results[:len(results)-t.keepWhole] {
		if !keep[entries[i].Seq] {
			cands = append(cands, entries[i])
		}
	}
	for _, s := range t.pick(inputSeq, prompt, cands) {
		keep[s] = true
	}
	// Oldest first, hiding until under budget; the kept set is never
	// hidden however far over we are.
	spent := total
	hide := map[int64]bool{}
	for _, e := range cands {
		if spent <= t.budget {
			break
		}
		text, _ := e.Data["text"].(string)
		if keep[e.Seq] || len(text) < inlineBelow {
			continue
		}
		hide[e.Seq] = true
		spent -= len(text)
	}
	if len(hide) == 0 {
		return loop.DefaultProject(entries)
	}
	if kernel.Verbose {
		kernel.Logf("memory-tier: hiding %d of %d outputs (%d -> %d chars)\n", len(hide), len(results), total, spent)
	}
	out := entries
	for _, e := range entries {
		if hide[e.Seq] {
			text, _ := e.Data["text"].(string)
			out = withText(out, e.Seq, t.placeholder(e.Seq, text))
		}
	}
	return loop.DefaultProject(out)
}

// withText is entries with one entry's text replaced, copying so the
// history's own slice and maps are never written.
func withText(entries []history.Entry, seq int64, text string) []history.Entry {
	out := slices.Clone(entries)
	for i, e := range out {
		if e.Seq != seq {
			continue
		}
		d := make(map[string]any, len(e.Data))
		maps.Copy(d, e.Data)
		d["text"] = text
		out[i].Data = d
	}
	return out
}

// placeholder is the one line a hidden output projects to.
func (t *Tier) placeholder(seq int64, text string) string {
	t.mu.Lock()
	line, ok := t.index[seq]
	t.mu.Unlock()
	if !ok {
		line = firstLine(text)
	}
	return fmt.Sprintf("[#%d hidden · %d chars · write <focus seq=%d> in your reply to see it in full this turn; do not guess its content] %s", seq, len(text), seq, line)
}

// pick asks the navigator, once per turn, which candidate outputs the
// prompt is about. Without a navigator, or on any failure, nothing.
func (t *Tier) pick(inputSeq int64, prompt string, cands []history.Entry) []int64 {
	if inputSeq == 0 || len(cands) == 0 || strings.TrimSpace(prompt) == "" {
		return nil
	}
	t.mu.Lock()
	got, done := t.picked[inputSeq]
	t.mu.Unlock()
	if done {
		return got
	}
	nav := t.navigator()
	if nav == nil {
		return nil
	}
	var b strings.Builder
	for _, e := range cands {
		text, _ := e.Data["text"].(string)
		fmt.Fprintf(&b, "#%d (%d chars): %s\n", e.Seq, len(text), t.placeholderLine(e.Seq, text))
	}
	ctx, cancel := context.WithTimeout(t.ctx, t.pickTimeout)
	defer cancel()
	reply, err := nav.Complete(ctx, pickPrompt, []llm.Message{{Role: "user",
		Content: "Request:\n" + prompt + "\n\nOutputs:\n" + b.String()}})
	var seqs []int64
	if err == nil {
		valid := map[int64]bool{}
		for _, e := range cands {
			valid[e.Seq] = true
		}
		for _, s := range parseSeqs(reply) {
			if valid[s] {
				seqs = append(seqs, s)
			}
		}
		if len(seqs) > t.topK {
			seqs = seqs[:t.topK]
		}
	}
	t.mu.Lock()
	t.picked[inputSeq] = seqs
	t.mu.Unlock()
	if err != nil {
		t.reportOnce(err)
	} else if len(seqs) > 0 && t.emit != nil {
		parts := make([]string, len(seqs))
		for i, s := range seqs {
			parts[i] = "#" + strconv.FormatInt(s, 10)
		}
		t.emit("memory", "memory-tier: brought back "+strings.Join(parts, ", ")+" for this turn")
	}
	return seqs
}

// navigator is the small model that writes index lines and picks.
func (t *Tier) navigator() llm.LLM {
	if t.nav == nil {
		return nil
	}
	return t.nav()
}

func (t *Tier) placeholderLine(seq int64, text string) string {
	t.mu.Lock()
	defer t.mu.Unlock()
	if line, ok := t.index[seq]; ok {
		return line
	}
	return firstLine(text)
}

const pickPrompt = `You are the memory navigator for a coding agent. You are given the user's latest request and a list of older tool outputs that are currently hidden from the agent, one per line as "#SEQ (size): summary". Reply with the SEQ numbers of the outputs the agent will need to answer this request, most important first, comma-separated, and nothing else. Reply "none" if none are needed. Prefer few.`

const indexPrompt = `You write one-line index entries for tool outputs in a coding agent's history. For each output below, reply with exactly one line: "#SEQ: summary". The summary is at most 120 characters, names what the output is (which command, file, or query) and the facts in it that a later step could need (counts, paths, errors, key values). No other text.`

// Index summarises outputs that have no index line yet. Called off
// the turn's goroutine; one run at a time, the rest skipped.
func (t *Tier) Index(entries []history.Entry) {
	nav := t.navigator()
	if nav == nil {
		return
	}
	t.mu.Lock()
	if t.busy {
		t.mu.Unlock()
		return
	}
	t.busy = true
	var todo []history.Entry
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		if e.Kind != "result" || len(text) < inlineBelow || t.index[e.Seq] != "" {
			continue
		}
		todo = append(todo, e)
	}
	t.mu.Unlock()
	defer func() {
		t.mu.Lock()
		t.busy = false
		t.mu.Unlock()
	}()
	for batch := range slices.Chunk(todo, indexBatch) {
		var b strings.Builder
		for _, e := range batch {
			text, _ := e.Data["text"].(string)
			if len(text) > indexInput {
				text = text[:indexInput/2] + "\n…\n" + text[len(text)-indexInput/2:]
			}
			fmt.Fprintf(&b, "=== #%d ===\n%s\n\n", e.Seq, text)
		}
		ctx, cancel := context.WithTimeout(t.ctx, 60*time.Second)
		reply, err := nav.Complete(ctx, indexPrompt, []llm.Message{{Role: "user", Content: b.String()}})
		cancel()
		if err != nil {
			t.reportOnce(err)
			return
		}
		lines := ParseIndex(reply)
		t.mu.Lock()
		for _, e := range batch {
			if l, ok := lines[e.Seq]; ok {
				t.index[e.Seq] = l
			}
		}
		t.mu.Unlock()
	}
}

// ParseIndex reads "#SEQ: summary" lines.
func ParseIndex(reply string) map[int64]string {
	out := map[int64]string{}
	for line := range strings.SplitSeq(reply, "\n") {
		line = strings.TrimSpace(line)
		rest, ok := strings.CutPrefix(line, "#")
		if !ok {
			continue
		}
		num, sum, ok := strings.Cut(rest, ":")
		if !ok {
			continue
		}
		n, err := strconv.ParseInt(strings.TrimSpace(num), 10, 64)
		if err != nil {
			continue
		}
		if sum = strings.TrimSpace(sum); sum != "" {
			out[n] = sum
		}
	}
	return out
}

// parseSeqs reads seq numbers out of a pick reply ("#12, 15" / "none").
func parseSeqs(reply string) []int64 {
	var out []int64
	for f := range strings.FieldsFuncSeq(reply, func(r rune) bool { return r < '0' || r > '9' }) {
		if n, err := strconv.ParseInt(f, 10, 64); err == nil && !slices.Contains(out, n) {
			out = append(out, n)
		}
	}
	return out
}

func (t *Tier) reportOnce(err error) {
	t.mu.Lock()
	first := !t.failed
	t.failed = true
	t.mu.Unlock()
	if first && t.emit != nil {
		t.emit("memory", "memory-tier: navigator failed — "+firstLine(err.Error()))
	}
}

func firstLine(text string) string {
	line, _, _ := strings.Cut(strings.TrimSpace(text), "\n")
	line = strings.TrimSpace(line)
	if len(line) > 120 {
		line = line[:117] + "…"
	}
	return line
}

type plugin struct{}

func init() {
	kernel.Register("memory-tier", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "memory-tier" }
func (plugin) Inject() []string { return []string{"llm", "history"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	// An init.js that called bough.project owns the projection
	// entirely; this row stands down rather than overwrite it.
	if _, err := kernel.Get[loop.Projection](kctx, "projection"); err == nil {
		return nil
	}
	// The navigator is the small model, never the conversation's own,
	// which would pay the main model's prices to write index lines.
	// Without an llm-small row the placeholders carry first lines and
	// no model is called.
	t := New(func() llm.LLM {
		if l, ok := llm.Small(kctx); ok {
			return l
		}
		return nil
	})
	for k, v := range cfg {
		n, err := toInt(v)
		if err != nil || n < 0 {
			return fmt.Errorf("memory-tier: %s must be a non-negative integer, got %v", k, v)
		}
		switch k {
		case "budget":
			t.budget = n
		case "keep_whole":
			t.keepWhole = n
		case "top_k":
			t.topK = n
		case "pick_timeout_seconds":
			t.pickTimeout = time.Duration(n) * time.Second
		default:
			return fmt.Errorf("memory-tier: unknown config key %q", k)
		}
	}
	h, err := kernel.Get[loop.History](kctx, "history")
	if err != nil {
		return fmt.Errorf("memory-tier: needs the history service")
	}
	ctx, cancel := context.WithCancel(context.Background())
	t.ctx = ctx
	kctx.Effect(cancel)
	t.emit = func(kind, text string) { kctx.Emit("loop/event", loop.Event{Kind: kind, Text: text}) }
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && (ev.Kind == "result" || ev.Kind == "done") {
			go t.Index(h.Entries())
		}
	})
	// The placeholder explains the focus protocol itself, so nothing
	// here touches the loop's prompt sections: this row mounts BEFORE
	// the loop (which reads "projection" at mount), and asking for a
	// loop service from here would remount both rows once the loop
	// appeared.
	kctx.Provide("projection", t)
	return nil
}

func toInt(v any) (int, error) {
	switch n := v.(type) {
	case int:
		return n, nil
	case int64:
		return int(n), nil
	case float64:
		return int(n), nil
	case string:
		return strconv.Atoi(n)
	}
	return 0, fmt.Errorf("not an integer: %v", v)
}
