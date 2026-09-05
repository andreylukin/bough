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
	Code  []string
	Clean bool // ended with "done", no "error", no "cancelled"
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
			turns = append(turns, Turn{Input: in})
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
				cur = nil
			}
		}
	}
	return turns
}

// recipeOf is the turn as a recipe, if it is one: a short prompt, one
// code block, a clean finish.
func recipeOf(t Turn, session, cwd string) (Recipe, bool) {
	if !t.Clean || len(t.Code) != 1 || strings.TrimSpace(t.Input) == "" || len(t.Input) > maxPhrasing {
		return Recipe{}, false
	}
	return Recipe{Phrasing: t.Input, Code: t.Code[0], Session: session, Cwd: cwd}, true
}

// Verdict is what the matcher would have done on one past turn.
type Verdict struct {
	Session  string
	Cwd      string
	Input    string
	Match    Match
	Fire     bool
	Actual   []string
	SameCode bool
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
			v := Verdict{Session: info.ID, Cwd: info.Cwd, Input: t.Input, Actual: t.Code}
			if m, ok := ix.Best(t.Input); ok {
				v.Match = m
				v.Fire = m.Score >= Threshold
				v.SameCode = len(t.Code) == 1 && strings.TrimSpace(t.Code[0]) == strings.TrimSpace(m.Recipe.Code)
			}
			out = append(out, v)
			if r, ok := recipeOf(t, info.ID, info.Cwd); ok {
				ix.Add(r)
			}
		}
	}
	return out, ix, nil
}

// Report writes the replay as text: the tally, then every would-fire
// with the recipe it would have run and whether the model ran the same.
func Report(w io.Writer, verdicts []Verdict, ix *Index, all bool) {
	fires, same := 0, 0
	for _, v := range verdicts {
		if v.Fire {
			fires++
			if v.SameCode {
				same++
			}
		}
	}
	fmt.Fprintf(w, "%d recipes learned, %d turns replayed, %d would fire, %d of those ran the same code\n", ix.Len(), len(verdicts), fires, same)
	for _, v := range verdicts {
		if !v.Fire && !all {
			continue
		}
		mark := " "
		switch {
		case v.Fire && v.SameCode:
			mark = "✓"
		case v.Fire:
			mark = "✗"
		}
		fmt.Fprintf(w, "\n%s %.2f  %q  (%s)\n", mark, v.Match.Score, oneLine(v.Input), shortCwd(v.Cwd))
		if v.Match.Recipe.Phrasing != "" {
			fmt.Fprintf(w, "       ↔ %q  (%s)\n", oneLine(v.Match.Recipe.Phrasing), shortCwd(v.Match.Recipe.Cwd))
			fmt.Fprintf(w, "       recipe: %s\n", oneLine(v.Match.Recipe.Code))
		}
		if v.Fire && !v.SameCode {
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
		m, ok := ix.Best(prompt)
		if !ok {
			fmt.Println("no match")
			return nil
		}
		fire := "below threshold"
		if m.Score >= Threshold {
			fire = "would fire"
		}
		fmt.Printf("%.2f  %s\n↔ %q  (%s)\n\n%s", m.Score, fire, m.Recipe.Phrasing, shortCwd(m.Recipe.Cwd), m.Recipe.Code)
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
