package recipes

import (
	"cmp"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// Turn is one finished user turn as the recipe extractor sees it.
type Turn struct {
	Input string
	Prev  string // the previous turn's input, "" for the first
	Code  []string
	Files []string // what the turn wrote, from its done entry
	Clean bool     // ended with "done", no "error", no "cancelled"
}

// maxPhrasing bounds what counts as a routine prompt: a paragraph of
// instructions is a task, not a phrase you will type again.
const maxPhrasing = 200

// Turns splits entries into user turns (steers fold into the turn they
// landed in).
func Turns(entries []history.Entry) []Turn {
	var turns []Turn
	var cur *Turn
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if steer, _ := e.Data["steer"].(bool); steer {
				continue
			}
			in, _ := e.Data["typed"].(string)
			if in == "" {
				in, _ = e.Data["text"].(string)
			}
			t := Turn{Input: in}
			if n := len(turns); n > 0 {
				t.Prev = turns[n-1].Input
			}
			turns = append(turns, t)
			cur = &turns[len(turns)-1]
		case "code":
			if cur != nil {
				c, _ := e.Data["text"].(string)
				cur.Code = append(cur.Code, c)
			}
		case "error", "cancelled":
			if cur != nil {
				cur.Clean = false
				cur = nil // whatever follows is not this turn's clean run
			}
		case "done":
			if cur != nil {
				cur.Clean = true
				if fs, ok := e.Data["files"].([]any); ok {
					for _, f := range fs {
						if p, ok := f.(string); ok {
							cur.Files = append(cur.Files, p)
						}
					}
				}
				cur = nil
			}
		}
	}
	return turns
}

// recipeOf is the turn as a recipe, if it is one: a short prompt, one
// code block, a clean finish.
func recipeOf(t Turn, session string, ctx Context) (Recipe, bool) {
	if !t.Clean || len(t.Code) != 1 || strings.TrimSpace(t.Input) == "" || len(t.Input) > maxPhrasing {
		return Recipe{}, false
	}
	return Recipe{Phrasing: t.Input, Code: t.Code[0], Session: session, Ctx: ctx}, true
}

// contextOf is the turn's situation: the session's directory, the
// paths the turn named (prompt, code, files written) and the checkouts
// they resolve to, the prompt before, the files it wrote. A turn that
// names no checkout inherits focus, the session's current one.
func contextOf(t Turn, cwd, focus string) Context {
	c := Context{Cwd: cwd, Root: gitRoot(cwd), Prev: t.Prev, Files: t.Files}
	bases := []string{cwd, focus}
	c.Paths = Paths(t.Input, bases...)
	for _, code := range t.Code {
		c.Paths = append(c.Paths, Paths(code, bases...)...)
	}
	c.Paths = append(c.Paths, t.Files...)
	c.Repos = Repos(c.Paths)
	if len(c.Repos) == 0 && focus != "" {
		c.Repos = []string{focus}
		c.Inherited = true
	}
	return c
}

// focusOf is the checkout a session is on after a turn: the last one
// its own paths named, else the one before. The session's working
// directory seeds it.
func focusOf(prev string, c Context) string {
	if !c.Inherited && len(c.Repos) > 0 {
		return c.Repos[len(c.Repos)-1]
	}
	return prev
}

// Verdict is what the matcher would have done on one past turn, under
// three gates: the same checkout (the real one), the same working
// directory, and words alone. Each is the best match under that gate.
type Verdict struct {
	Session string
	Ctx     Context // the whole turn, code included: what the recipe records
	Ask     Context // what was known before the model answered: what the gate sees
	Input   string
	Actual  []string
	Repo    Outcome // a checkout the prompt names, or the session's focus: what would actually fire
	Dir     Outcome // same cwd
	Words   Outcome // no context at all
}

// askOf is the context before the model answers: the prompt's own
// paths, else the session's focus so far.
func askOf(t Turn, cwd, focus string) Context {
	c := Context{Cwd: cwd, Root: gitRoot(cwd), Prev: t.Prev}
	c.Paths = Paths(t.Input, cwd, focus)
	c.Repos = Repos(c.Paths)
	if len(c.Repos) == 0 && focus != "" {
		c.Repos = []string{focus}
		c.Inherited = true
	}
	return c
}

// Outcome is one gate's answer.
type Outcome struct {
	Match    Match
	Fire     bool
	SameCode bool
}

func outcome(ix *Index, t Turn, accept func(Recipe) bool) Outcome {
	m, ok := ix.Best(t.Input, accept)
	if !ok {
		return Outcome{}
	}
	return Outcome{
		Match:    m,
		Fire:     m.Score >= Threshold,
		SameCode: len(t.Code) == 1 && strings.TrimSpace(t.Code[0]) == strings.TrimSpace(m.Recipe.Code),
	}
}

// Replay walks every session oldest first and scores each turn against
// the recipes learned from the turns before it — what a live matcher
// would have seen at the time. It returns the verdicts and the final
// index.
func Replay(dir string) ([]Verdict, *Index, error) {
	infos, err := history.List(dir)
	if err != nil {
		return nil, nil, err
	}
	// List is newest first; the replay must learn in the order things
	// happened. Two sessions whose files land in the same filesystem
	// tick tie on mtime, and a tie left to the sort is an arbitrary
	// order — which session's recipes exist when the other's turns are
	// scored, and so the whole verdict. The id breaks it: a UUIDv7
	// sorts oldest-first by its leading millisecond timestamp.
	slices.SortFunc(infos, func(a, b history.SessionInfo) int {
		return cmp.Or(a.ModTime.Compare(b.ModTime), cmp.Compare(a.ID, b.ID))
	})
	ix := NewIndex(nil)
	var out []Verdict
	for _, info := range infos {
		entries, err := history.Read(info.Path)
		if err != nil {
			continue
		}
		focus := gitRoot(info.Cwd)
		for _, t := range Turns(entries) {
			if strings.TrimSpace(t.Input) == "" || strings.HasPrefix(t.Input, "/") {
				continue
			}
			// The gate sees what a live matcher would: the prompt and
			// the session so far, not the code the model has not
			// written yet.
			ask := askOf(t, info.Cwd, focus)
			ctx := contextOf(t, info.Cwd, focus)
			focus = focusOf(focus, ctx)
			v := Verdict{Session: info.ID, Ctx: ctx, Ask: ask, Input: t.Input, Actual: t.Code}
			v.Repo = outcome(ix, t, func(r Recipe) bool { return ask.SameRepo(r.Ctx) })
			v.Dir = outcome(ix, t, func(r Recipe) bool { return ask.SameDir(r.Ctx) })
			v.Words = outcome(ix, t, nil)
			out = append(out, v)
			if r, ok := recipeOf(t, info.ID, ctx); ok {
				ix.Add(r)
			}
		}
	}
	return out, ix, nil
}

// Report writes the replay as text: the tally under each gate, then
// every turn the checkout gate would fire on, with the recipe it
// would run, its context, and whether the model ran the same code.
func Report(w io.Writer, verdicts []Verdict, ix *Index, all bool) {
	tally := func(pick func(Verdict) Outcome) (fires, same int) {
		for _, v := range verdicts {
			if o := pick(v); o.Fire {
				fires++
				if o.SameCode {
					same++
				}
			}
		}
		return
	}
	fmt.Fprintf(w, "%d recipes learned, %d turns replayed\n", ix.Len(), len(verdicts))
	for _, g := range []struct {
		name string
		pick func(Verdict) Outcome
	}{
		{"same checkout (the gate)", func(v Verdict) Outcome { return v.Repo }},
		{"same directory", func(v Verdict) Outcome { return v.Dir }},
		{"words only", func(v Verdict) Outcome { return v.Words }},
	} {
		fires, same := tally(g.pick)
		fmt.Fprintf(w, "  %-27s %3d would fire, %3d ran the same code\n", g.name, fires, same)
	}
	for _, v := range verdicts {
		o := v.Repo
		if !o.Fire && !all {
			continue
		}
		mark := " "
		switch {
		case o.Fire && o.SameCode:
			mark = "✓"
		case o.Fire:
			mark = "✗"
		}
		fmt.Fprintf(w, "\n%s %.2f  %q\n", mark, o.Match.Score, oneLine(v.Input))
		fmt.Fprintf(w, "       %s", v.Ask.where())
		if v.Ctx.Prev != "" {
			fmt.Fprintf(w, "  after %q", oneLine(v.Ctx.Prev))
		}
		fmt.Fprintln(w)
		r := o.Match.Recipe
		if r.Phrasing != "" {
			fmt.Fprintf(w, "       ↔ %q  %s", oneLine(r.Phrasing), r.Ctx.where())
			if r.Ctx.Prev != "" {
				fmt.Fprintf(w, "  after %q", oneLine(r.Ctx.Prev))
			}
			fmt.Fprintln(w)
			fmt.Fprintf(w, "       recipe: %s\n", oneLine(r.Code))
			if len(r.Ctx.Paths) > 0 {
				fmt.Fprintf(w, "       paths:  %s\n", oneLine(strings.Join(shortAll(r.Ctx.Paths), " ")))
			}
		}
		if o.Fire && !o.SameCode {
			fmt.Fprintf(w, "       actual: %d block(s)", len(v.Actual))
			if len(v.Actual) > 0 {
				fmt.Fprintf(w, ": %s", oneLine(v.Actual[0]))
			}
			fmt.Fprintln(w)
		}
	}
}

func oneLine(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	if len(s) > 80 {
		s = s[:80] + "…"
	}
	return s
}

func shortAll(ps []string) []string {
	out := make([]string, len(ps))
	for i, p := range ps {
		out[i] = shortCwd(p)
	}
	return out
}

func shortCwd(s string) string {
	if s == "" {
		return "?"
	}
	if home, err := os.UserHomeDir(); err == nil {
		s = strings.Replace(s, home, "~", 1)
	}
	return s
}

type plugin struct{}

func init() {
	kernel.Register("recipes", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "recipes" }
func (plugin) Inject() []string { return nil }

// Apply is a no-op: the plugin is its CLI command; there is no row.
func (plugin) Apply(*kernel.Context, map[string]any) error { return nil }

func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "recipes",
		Usage:   "[--all] | try <prompt>",
		Summary: "replay history through the no-model recipe matcher: what would have fired, and would it have been right",
		Run:     runCLI,
	}}
}

func runCLI(_ map[string]any, args []string) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	dir := filepath.Join(home, ".bough", "history")
	if len(args) > 0 && args[0] == "try" {
		prompt := strings.Join(args[1:], " ")
		if strings.TrimSpace(prompt) == "" {
			return errors.New("usage: bough recipes try <prompt>")
		}
		_, ix, err := Replay(dir)
		if err != nil {
			return err
		}
		cwd, _ := os.Getwd()
		here := askOf(Turn{Input: prompt}, cwd, gitRoot(cwd))
		fmt.Printf("asking %s\n", here.where())
		m, ok := ix.Best(prompt, func(r Recipe) bool { return here.SameRepo(r.Ctx) })
		if !ok {
			fmt.Println("no recipe from that checkout matches")
			if m, ok := ix.Best(prompt, nil); ok {
				fmt.Printf("nearest anywhere: %.2f %q %s\n", m.Score, oneLine(m.Recipe.Phrasing), m.Recipe.Ctx.where())
			}
			return nil
		}
		fire := "below threshold"
		if m.Score >= Threshold {
			fire = "would fire"
		}
		fmt.Printf("%.2f  %s\n↔ %q  %s\n\n%s", m.Score, fire, m.Recipe.Phrasing, m.Recipe.Ctx.where(), m.Recipe.Code)
		return nil
	}
	all := false
	for _, a := range args {
		switch a {
		case "--all", "-a":
			all = true
		default:
			return fmt.Errorf("usage: bough recipes [--all] | try <prompt> (got %q)", a)
		}
	}
	verdicts, ix, err := Replay(dir)
	if err != nil {
		return err
	}
	Report(os.Stdout, verdicts, ix, all)
	return nil
}
