package title

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

const summarizeUsage = "[session-id...] [--all] [--dry-run] [--max-turns N] [--model M]"

// defaultModel is the llm-small row's model: the command reads no other
// row's config, so it names the one the log is written with by default.
const defaultModel = "openai/gpt-5.6-luna"

// Commands implements kernel.Commander: `bough summarize`.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "summarize",
		Usage:   summarizeUsage,
		Summary: "backfill the per-turn log and final title/summary of sessions that have none",
		Run:     runSummarize,
	}}
}

type summarizeOpts struct {
	ids      []string
	all, dry bool
	maxTurns int
	model    string
}

func parseSummarize(args []string) (summarizeOpts, error) {
	o := summarizeOpts{model: defaultModel}
	for i := 0; i < len(args); i++ {
		switch a := args[i]; a {
		case "--all":
			o.all = true
		case "--dry-run":
			o.dry = true
		case "--max-turns", "--model":
			if i+1 >= len(args) {
				return o, fmt.Errorf("%s needs a value", a)
			}
			i++
			if a == "--model" {
				o.model = args[i]
				continue
			}
			n, err := strconv.Atoi(args[i])
			if err != nil || n < 1 {
				return o, fmt.Errorf("--max-turns wants a positive number, got %q", args[i])
			}
			o.maxTurns = n
		default:
			if strings.HasPrefix(a, "-") {
				return o, fmt.Errorf("unknown flag %s (usage: bough summarize %s)", a, summarizeUsage)
			}
			o.ids = append(o.ids, a)
		}
	}
	if len(o.ids) == 0 && !o.all {
		return o, fmt.Errorf("name sessions or pass --all (usage: bough summarize %s)", summarizeUsage)
	}
	return o, nil
}

func runSummarize(_ map[string]any, args []string) error {
	o, err := parseSummarize(args)
	if err != nil {
		return err
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	newLLM := func() (llm.LLM, error) {
		c := kernel.NewContext()
		if err := c.Mount([]kernel.Row{{ID: llm.SmallKey, Plugin: "llm-openrouter",
			Config: map[string]any{"service": llm.SmallKey, "model": o.model}}}); err != nil {
			return nil, err
		}
		l, err := kernel.Get[llm.LLM](c, llm.SmallKey)
		if err != nil {
			return nil, err
		}
		if r, ok := l.(interface{ Ready() error }); ok {
			if err := r.Ready(); err != nil {
				return nil, err
			}
		}
		return l, nil
	}
	return Backfill(context.Background(), os.Stdout, filepath.Join(home, ".bough", "history"), o, newLLM)
}

// sessionPaths resolves ids (or unique id prefixes) to session files,
// or every session under --all.
func sessionPaths(dir string, o summarizeOpts) ([]string, error) {
	if o.all {
		return filepath.Glob(filepath.Join(dir, "*.jsonl"))
	}
	var out []string
	for _, id := range o.ids {
		m, _ := filepath.Glob(filepath.Join(dir, filepath.Base(id)+"*.jsonl"))
		switch len(m) {
		case 1:
			out = append(out, m[0])
		case 0:
			return nil, fmt.Errorf("no session %q in %s", id, dir)
		default:
			return nil, fmt.Errorf("%q names %d sessions", id, len(m))
		}
	}
	return out, nil
}

// tally is what one session's backfill spent.
type tally struct {
	calls int
	usage llm.Usage
}

func (t tally) String() string {
	s := fmt.Sprintf("%d calls", t.calls)
	if t.usage.InputTokens+t.usage.OutputTokens > 0 {
		s += fmt.Sprintf(", %d in / %d out tokens", t.usage.InputTokens, t.usage.OutputTokens)
	}
	if t.usage.Priced || t.usage.Cost > 0 {
		s += fmt.Sprintf(", $%.4f", t.usage.Cost)
	}
	return s
}

// Backfill rebuilds the running log and final name of each session that
// has no log yet, four sessions at a time, printing each as it finishes.
// A dry run prints and writes nothing.
func Backfill(ctx context.Context, w io.Writer, dir string, o summarizeOpts, newLLM func() (llm.LLM, error)) error {
	paths, err := sessionPaths(dir, o)
	if err != nil {
		return err
	}
	var (
		mu    sync.Mutex
		total tally
		errs  []error
		wg    sync.WaitGroup
		sem   = make(chan struct{}, 4)
	)
	for _, p := range paths {
		wg.Add(1)
		sem <- struct{}{}
		go func() {
			defer func() { <-sem; wg.Done() }()
			var b strings.Builder
			t, err := backfillOne(ctx, &b, p, o, newLLM)
			mu.Lock()
			defer mu.Unlock()
			io.WriteString(w, b.String())
			total.calls += t.calls
			total.usage.InputTokens += t.usage.InputTokens
			total.usage.OutputTokens += t.usage.OutputTokens
			total.usage.Cost += t.usage.Cost
			total.usage.Priced = total.usage.Priced || t.usage.Priced
			if err != nil {
				fmt.Fprintf(w, "%s: %v\n\n", id(p), err)
				errs = append(errs, err)
			}
		}()
	}
	wg.Wait()
	fmt.Fprintf(w, "total: %s\n", total)
	return errors.Join(errs...)
}

func id(path string) string { return strings.TrimSuffix(filepath.Base(path), ".jsonl") }

func backfillOne(ctx context.Context, w io.Writer, path string, o summarizeOpts, newLLM func() (llm.LLM, error)) (tally, error) {
	var t tally
	entries, err := history.Read(path)
	if err != nil {
		return t, err
	}
	if _, logged := turnLog(entries); logged > 0 {
		if !o.all {
			fmt.Fprintf(w, "%s: already summarized, skipped\n\n", id(path))
		}
		return t, nil
	}
	turns := Turns(entries)
	if len(turns) == 0 {
		if !o.all {
			fmt.Fprintf(w, "%s: no turns, skipped\n\n", id(path))
		}
		return t, nil
	}
	if o.maxTurns > 0 && len(turns) > o.maxTurns {
		turns = turns[:o.maxTurns]
	}
	l, err := newLLM()
	if err != nil {
		return t, err
	}
	defer func() {
		if r, ok := l.(llm.UsageReporter); ok {
			t.usage = r.Usage()
		}
	}()
	fmt.Fprintf(w, "%s (%d turns)\n", id(path), len(turns))
	var lines, texts []string
	var nums []int
	for i, tr := range turns {
		t.calls++
		reply, err := call(ctx, l, TurnPrompt, TurnInput(lines, tr))
		if err != nil {
			return t, fmt.Errorf("turn %d: %w", i+1, err)
		}
		line := CleanLine(reply)
		if line == "" {
			continue
		}
		lines = append(lines, fmt.Sprintf("%d. %s", i+1, line))
		texts, nums = append(texts, line), append(nums, i+1)
		fmt.Fprintf(w, "  %s\n", lines[len(lines)-1])
	}
	if len(lines) == 0 {
		return t, fmt.Errorf("the model wrote no log lines")
	}
	t.calls++
	reply, err := call(ctx, l, FinalPrompt, "Running log:\n"+strings.Join(lines, "\n"))
	if err != nil {
		return t, fmt.Errorf("final: %w", err)
	}
	title, summary := Parse(reply)
	fmt.Fprintf(w, "  Title: %s\n  Summary: %s\n", title, summary)
	if o.dry {
		fmt.Fprintf(w, "  [dry run, nothing written]\n\n")
		return t, nil
	}
	s, err := history.OpenExisting(path)
	if err != nil {
		return t, err
	}
	for i, text := range texts {
		s.Append("turn-summary", map[string]any{"text": text, "turn": nums[i]})
	}
	if title != "" {
		data := map[string]any{"text": title, "turn": nums[len(nums)-1], "final": true}
		if summary != "" {
			data["summary"] = summary
		}
		s.Append("title", data)
	}
	if err := s.Close(); err != nil {
		return t, err
	}
	if err := s.TakeErr(); err != nil {
		return t, err
	}
	fmt.Fprintln(w)
	return t, nil
}
