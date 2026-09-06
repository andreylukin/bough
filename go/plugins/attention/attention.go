// Package attention is the attention board: the work around me, from
// the memory graph, as three columns by whose turn it is. NEEDS ME is
// what awaits me; IN MOTION is what a session is working right now
// (pr-watch locks); WAITING ON OTHERS is mine, open, in someone else's
// hands. The ui draws it; this row only decides what goes where.
//
// Row config: sticky: true pins the board at the top of every session
// from the start; false (the default) leaves it behind /current-work.
package attention

import (
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/graph"
	"github.com/andreylukin/bough/plugins/prwatch"
)

// Item is one row of the board.
type Item struct {
	Key     string // graph key: bough#64, LIN-482
	Kind    string // pr, ticket, thread, page
	Title   string
	Status  string // open, ci failing, ...
	Detail  string // the second line: what it asks, of whom, since when
	URL     string
	Since   time.Time // when this party's turn began (the source's clock)
	Session string    // IN MOTION: the session working it ("" elsewhere)
	Count   int       // a stack of Count similar items folded into one row (0 = single)
}

// Board is the world by whose turn it is.
type Board struct {
	Me        []Item // awaits me
	Motion    []Item // a session is on it
	Others    []Item // mine, awaiting someone else (or nobody recorded)
	Collected time.Time
	Err       string // why the board is empty, when it is
}

// Empty reports a board with nothing to show.
func (b Board) Empty() bool { return len(b.Me)+len(b.Motion)+len(b.Others) == 0 }

// lockLister is the pr-watch seam: what any session is working now.
type lockLister interface{ Working() []prwatch.Working }

// working is one pr-watch lock.
type working = prwatch.Working

// Service builds boards.
type Service struct {
	graph  *graph.Service
	locks  func() []working
	sticky bool
	Now    func() time.Time
}

// Sticky is the row's flag: pin the board from the first frame.
func (s *Service) Sticky() bool { return s.sticky }

// Board is the current board. Reads the graph; cheap (one person's
// neighbourhood), but a file read, so the ui calls it on a timer.
func (s *Service) Board() Board {
	now := s.Now()
	var b Board
	w, err := s.graph.World()
	if err != nil {
		b.Err = err.Error()
		return b
	}
	if w.Fresh > 0 {
		b.Collected = time.Unix(w.Fresh, 0)
	}
	var locks []working
	if s.locks != nil {
		locks = s.locks()
	}
	inMotion := func(e graph.Entity) (working, bool) {
		for _, l := range locks {
			// Lock keys are owner/name#n; graph keys are name#n.
			_, short, _ := strings.Cut(l.Key, "/")
			if short == e.Key || l.Key == e.Key {
				return l, true
			}
		}
		return working{}, false
	}
	seen := map[string]bool{}
	place := func(e graph.Entity, tail string) {
		if seen[e.Key] {
			return
		}
		seen[e.Key] = true
		it := item(e, now)
		if l, ok := inMotion(e); ok {
			it.Session = l.Session
			it.Since = l.Since
			it.Detail = l.What + " · session " + short(l.Session, 4)
			b.Motion = append(b.Motion, it)
			return
		}
		it.Detail = tail
		if _, awaited := awaitedBy(w, e); awaited {
			b.Me = append(b.Me, it)
		} else {
			b.Others = append(b.Others, it)
		}
	}
	for _, e := range w.AwaitsMe {
		place(e, ask(e))
	}
	for _, e := range w.Mine {
		who := "next: ?"
		if names := w.Awaiting[e.ID]; len(names) > 0 {
			var ns []string
			for _, p := range names {
				ns = append(ns, first(p.Title, p.Key))
			}
			who = "awaits " + strings.Join(ns, ", ")
		}
		place(e, who)
	}
	b.Me = stack(b.Me)
	byAge := func(a, c Item) int { return a.Since.Compare(c.Since) }
	slices.SortStableFunc(b.Me, byAge)
	slices.SortStableFunc(b.Others, byAge)
	if b.Empty() && b.Err == "" {
		b.Err = "nothing open around you"
		if w.Fresh == 0 {
			b.Err = "no collector has run: bough collect"
		}
	}
	return b
}

// awaitedBy reports whether e is in the awaits-me list.
func awaitedBy(w graph.World, e graph.Entity) (graph.Entity, bool) {
	for _, a := range w.AwaitsMe {
		if a.ID == e.ID {
			return a, true
		}
	}
	return graph.Entity{}, false
}

// item is the entity as a row: title trimmed, the CI/review facts of
// the summary kept as status.
func item(e graph.Entity, now time.Time) Item {
	it := Item{Key: e.Key, Kind: e.Kind, Title: first(e.Title, e.Key), URL: e.URL, Status: e.Status}
	if e.UpdatedAt > 0 {
		it.Since = time.Unix(e.UpdatedAt, 0)
	} else {
		it.Since = now
	}
	if strings.Contains(e.Summary, "ci failing") {
		it.Status = "ci failing"
	} else if strings.Contains(e.Summary, "ci green") {
		it.Status = "ci green"
	}
	return it
}

// ask is what an item awaiting me asks: the summary's first clause
// that is not a CI fact ("review required").
func ask(e graph.Entity) string {
	for _, part := range strings.Split(e.Summary, ",") {
		p := strings.TrimSpace(part)
		if p == "" || strings.HasPrefix(p, "ci ") || strings.HasPrefix(p, "branch ") {
			continue
		}
		return p
	}
	return "awaits you"
}

// stack folds dependency-bot PRs of one repo into a single row: twelve
// bumps must not outweigh two real projects.
func stack(items []Item) []Item {
	groups := map[string][]int{}
	var order []string
	for i, it := range items {
		if !bot(it) {
			continue
		}
		repo, _, _ := strings.Cut(it.Key, "#")
		if _, ok := groups[repo]; !ok {
			order = append(order, repo)
		}
		groups[repo] = append(groups[repo], i)
	}
	drop := map[int]bool{}
	var out []Item
	for _, repo := range order {
		idx := groups[repo]
		if len(idx) < 2 {
			continue
		}
		head := items[idx[0]]
		head.Key = repo + " · deps"
		head.Title = repo + " · dependency updates"
		head.Count = len(idx)
		head.URL = ""
		failing := 0
		for _, i := range idx {
			drop[i] = true
			if items[i].Status == "ci failing" {
				failing++
			}
			if items[i].Since.Before(head.Since) {
				head.Since = items[i].Since
			}
		}
		if failing > 0 {
			head.Status = fmt.Sprintf("ci failing ×%d", failing)
		}
		out = append(out, head)
	}
	for i, it := range items {
		if !drop[i] {
			out = append(out, it)
		}
	}
	return out
}

// bot is a dependency-bot PR: the graph has no author, the branch name
// in the summary does.
func bot(it Item) bool {
	t := strings.ToLower(it.Title)
	return strings.HasPrefix(t, "chore(deps") || strings.HasPrefix(t, "build(deps") || strings.HasPrefix(t, "chore(ci): bump") || strings.HasPrefix(t, "bump ")
}

func first(a, b string) string {
	if a != "" {
		return a
	}
	return b
}

func short(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}

// Age is the fill of an item's bar: 0..8 cells on a log scale, one at
// an hour, four at a day, eight at a week. The oldest debt is the
// brightest thing on screen without reading a date.
func Age(since, now time.Time) int {
	h := now.Sub(since).Hours()
	if h < 1 {
		return 0
	}
	cells := 1 + 7*math.Log(h)/math.Log(168)
	return max(0, min(8, int(cells+0.5)))
}

// ---------- plugin ----------

type plugin struct{}

func init() {
	kernel.Register("attention", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "attention" }
func (plugin) Inject() []string { return []string{"graph"} }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	s := &Service{Now: time.Now}
	for k, v := range cfg {
		switch k {
		case "sticky":
			// A bool from yaml, a string from --set.
			switch b := v.(type) {
			case bool:
				s.sticky = b
			case string:
				p, err := strconv.ParseBool(b)
				if err != nil {
					return fmt.Errorf("attention: sticky must be true or false, got %q", b)
				}
				s.sticky = p
			default:
				return fmt.Errorf("attention: sticky must be true or false, got %v", v)
			}
		default:
			return fmt.Errorf("attention: unknown config key %q", k)
		}
	}
	g, err := kernel.Get[*graph.Service](kctx, "graph")
	if err != nil {
		return errors.New("attention: needs the graph row")
	}
	s.graph = g
	if l, err := kernel.Get[lockLister](kctx, "pr-watch"); err == nil {
		s.locks = l.Working
	}
	kctx.Provide("attention", s)
	if reg, err := kernel.Get[*commands.Registry](kctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "current-work", Summary: "toggle the attention board: what awaits you, what is in motion, what waits on others"}
		if err := reg.Register(info, func(string) (string, error) { return "", commands.ActionBoard }); err == nil {
			kctx.Effect(func() { reg.Unregister("current-work") })
		}
	}
	return nil
}
