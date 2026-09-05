package recipes

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// History is the seam: this session's entries and where they live.
type History interface {
	Entries() []history.Entry
	Path() string
}

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

// Verdict is one shadow-log line.
type Verdict struct {
	At       time.Time `json:"at"`
	Session  string    `json:"session"`
	Input    string    `json:"input"`
	Fire     bool      `json:"fire"`
	Score    float64   `json:"score"`
	Phrasing string    `json:"phrasing,omitempty"`
	From     string    `json:"from,omitempty"`
	FromCwd  string    `json:"from_cwd,omitempty"`
	Cwd      string    `json:"cwd,omitempty"`
	Recipe   string    `json:"recipe_code,omitempty"`
	Actual   []string  `json:"actual_code"`
	SameCode bool      `json:"same_code"`
}

// Shadow watches turns and scores them.
type Shadow struct {
	hist    History
	session string
	cwd     string
	logPath string

	mu       sync.Mutex
	ix       *Index
	seen     int // turns scored
	scored   []Verdict
	lastTurn int // turns already scored in this session
}

// Load reads every other session under dir into an index.
func Load(dir, skip string) *Index {
	ix := NewIndex(nil)
	infos, err := history.List(dir)
	if err != nil {
		return ix
	}
	for _, info := range infos {
		if info.Path == skip {
			continue
		}
		entries, err := history.Read(info.Path)
		if err != nil {
			continue
		}
		for _, t := range Turns(entries) {
			if r, ok := recipeOf(t, info.ID, info.Cwd); ok {
				ix.Add(r)
			}
		}
	}
	return ix
}

// onDone scores every not-yet-scored finished turn of this session,
// then adds the turn to the index so a repeat later this session can
// hit it.
func (s *Shadow) onDone() {
	s.mu.Lock()
	defer s.mu.Unlock()
	turns := Turns(s.hist.Entries())
	for i := s.lastTurn; i < len(turns); i++ {
		t := turns[i]
		if !t.Clean && i == len(turns)-1 {
			return // still running (a done for a steer, say); score it next time
		}
		s.lastTurn = i + 1
		if strings.TrimSpace(t.Input) == "" || strings.HasPrefix(t.Input, "/") {
			continue
		}
		v := Verdict{At: time.Now(), Session: s.session, Cwd: s.cwd, Input: t.Input, Actual: t.Code}
		if m, ok := s.ix.Best(t.Input); ok {
			v.Score = m.Score
			v.Phrasing = m.Recipe.Phrasing
			v.From = m.Recipe.Session
			v.FromCwd = m.Recipe.Cwd
			v.Recipe = m.Recipe.Code
			v.Fire = m.Score >= Threshold
			v.SameCode = len(t.Code) == 1 && strings.TrimSpace(t.Code[0]) == strings.TrimSpace(m.Recipe.Code)
		}
		s.seen++
		s.scored = append(s.scored, v)
		s.append(v)
		if r, ok := recipeOf(t, s.session, s.cwd); ok {
			s.ix.Add(r)
		}
	}
}

func (s *Shadow) append(v Verdict) {
	if s.logPath == "" {
		return
	}
	f, err := os.OpenFile(s.logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(v)
	f.Write(append(b, '\n'))
}

// Report is the /recipes text: the tally and the last few would-fires.
func (s *Shadow) Report() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	fires, same := 0, 0
	for _, v := range s.scored {
		if v.Fire {
			fires++
			if v.SameCode {
				same++
			}
		}
	}
	var b strings.Builder
	fmt.Fprintf(&b, "recipes: %d in index, %d turns scored this session, %d would fire (%d ran the same code)\n", s.ix.Len(), s.seen, fires, same)
	if s.logPath != "" {
		fmt.Fprintf(&b, "log: %s\n", s.logPath)
	}
	n := 0
	for i := len(s.scored) - 1; i >= 0 && n < 5; i-- {
		v := s.scored[i]
		if !v.Fire {
			continue
		}
		n++
		fmt.Fprintf(&b, "\n%.2f  %q\n   ↔ %q (%s)\n   recipe: %s\n", v.Score, oneLine(v.Input), oneLine(v.Phrasing), v.FromCwd, oneLine(v.Recipe))
	}
	return strings.TrimRight(b.String(), "\n")
}

func oneLine(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	if len(s) > 80 {
		s = s[:80] + "…"
	}
	return s
}

type plugin struct{}

func init() {
	kernel.Register("recipes", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "recipes" }
func (plugin) Inject() []string { return []string{"history"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("recipes: unknown config key %q", k)
	}
	h, err := kernel.Get[History](kctx, "history")
	if err != nil {
		return fmt.Errorf("recipes: needs the history service")
	}
	s := &Shadow{hist: h, ix: NewIndex(nil)}
	s.cwd, _ = os.Getwd()
	if p := h.Path(); p != "" {
		s.session = strings.TrimSuffix(filepath.Base(p), ".jsonl")
		s.logPath = filepath.Join(filepath.Dir(p), "..", "recipes.log")
		// Reading every session is I/O the turn should not wait on.
		go func() {
			ix := Load(filepath.Dir(p), p)
			s.mu.Lock()
			for _, r := range s.ix.recipes {
				ix.Add(r)
			}
			s.ix = ix
			s.mu.Unlock()
		}()
	}
	kctx.Provide("recipes", s)
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "done" {
			go s.onDone()
		}
	})
	if reg, err := kernel.Get[*commands.Registry](kctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "recipes", Summary: "Shadow-mode recipe matcher tally"}
		if err := reg.Register(info, func(string) (string, error) { return s.Report(), nil }); err != nil {
			return err
		}
		kctx.Effect(func() { reg.Unregister("recipes") })
	}
	return nil
}
