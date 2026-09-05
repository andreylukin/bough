package recipes

import (
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

// contextOf is the turn's situation: the session's directory, its
// checkout (walked up from the directory now, so a deleted checkout
// has none), the prompt before, the files it wrote.
func contextOf(t Turn, cwd string) Context {
	return Context{Cwd: cwd, Root: gitRoot(cwd), Prev: t.Prev, Files: t.Files}
}

// gitRoot is the nearest ancestor of dir holding a .git, or "".
func gitRoot(dir string) string {
	for d := dir; d != "" && d != "/"; d = filepath.Dir(d) {
		if _, err := os.Stat(filepath.Join(d, ".git")); err == nil {
			return d
		}
		if filepath.Dir(d) == d {
			break
		}
	}
	return ""
}

// Verdict is what the matcher would have done on one past turn, under
// three gates: the same directory (the real one), the same checkout,
// and words alone. Each is the best match under that gate.
type Verdict struct {
	Session string
	Ctx     Context
	Input   string
	Actual  []string
	Dir     Outcome // same cwd: what would actually fire
	Repo    Outcome // same git root, any directory
	Words   Outcome // no context at all
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
	// happened.
	slices.SortFunc(infos, func(a, b history.SessionInfo) int { return a.ModTime.Compare(b.ModTime) })
	ix := NewIndex(nil)
	var out []Verdict
	for _, info := range infos {
		entries, err := history.Read(info.Path)
		if err != nil {
			continue
		}
		for _, t := range Turns(entries) {
			if strings.TrimSpace(t.Input) == "" || strings.HasPrefix(t.Input, "/") {
				continue
			}
			ctx := contextOf(t, info.Cwd)
			v := Verdict{Session: info.ID, Ctx: ctx, Input: t.Input, Actual: t.Code}
			v.Dir = outcome(ix, t, func(r Recipe) bool { return ctx.SameDir(r.Ctx) })
			v.Repo = outcome(ix, t, func(r Recipe) bool { return ctx.SameRepo(r.Ctx) })
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
// every turn the directory gate would fire on, with the recipe it
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
		{"same directory (the gate)", func(v Verdict) Outcome { return v.Dir }},
		{"same checkout", func(v Verdict) Outcome { return v.Repo }},
		{"words only", func(v Verdict) Outcome { return v.Words }},
	} {
		fires, same := tally(g.pick)
		fmt.Fprintf(w, "  %-27s %3d would fire, %3d ran the same code\n", g.name, fires, same)
	}
	for _, v := range verdicts {
		o := v.Dir
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
		fmt.Fprintf(w, "       in %s", shortCwd(v.Ctx.Cwd))
		if v.Ctx.Prev != "" {
			fmt.Fprintf(w, "  after %q", oneLine(v.Ctx.Prev))
		}
		fmt.Fprintln(w)
		r := o.Match.Recipe
		if r.Phrasing != "" {
			fmt.Fprintf(w, "       ↔ %q", oneLine(r.Phrasing))
			if r.Ctx.Prev != "" {
				fmt.Fprintf(w, "  after %q", oneLine(r.Ctx.Prev))
			}
			fmt.Fprintln(w)
			fmt.Fprintf(w, "       recipe: %s\n", oneLine(r.Code))
			if len(r.Ctx.Files) > 0 {
				fmt.Fprintf(w, "       wrote:  %s\n", oneLine(strings.Join(r.Ctx.Files, " ")))
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
		here := Context{Cwd: cwd, Root: gitRoot(cwd)}
		m, ok := ix.Best(prompt, func(r Recipe) bool { return here.SameDir(r.Ctx) })
		if !ok {
			fmt.Printf("no recipe learned in %s matches\n", shortCwd(cwd))
			if m, ok := ix.Best(prompt, nil); ok {
				fmt.Printf("nearest anywhere: %.2f %q in %s\n", m.Score, oneLine(m.Recipe.Phrasing), shortCwd(m.Recipe.Ctx.Cwd))
			}
			return nil
		}
		fire := "below threshold"
		if m.Score >= Threshold {
			fire = "would fire"
		}
		fmt.Printf("%.2f  %s\n↔ %q  in %s\n\n%s", m.Score, fire, m.Recipe.Phrasing, shortCwd(m.Recipe.Ctx.Cwd), m.Recipe.Code)
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
