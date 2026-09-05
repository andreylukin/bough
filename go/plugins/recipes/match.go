// Package recipes is the "recipes" plugin: an offline experiment in
// answering routine prompts without a model call (after NPC-Forge /
// TERMy). A recipe is a past one-block turn: what you typed and the
// single code block the model ran, which then finished cleanly.
// `bough recipes` replays every session through the matcher and says
// what would have fired and whether the model then ran the same code.
// Nothing runs during a live session.
package recipes

import (
	"math"
	"slices"
	"strings"
	"unicode"
)

// Recipe is one memorised turn: the words, the code, and the situation
// it happened in.
type Recipe struct {
	Phrasing string `json:"phrasing"`
	Code     string `json:"code"`
	Session  string `json:"session"`
	Ctx      Context
}

// Context is everything known about a turn besides its words. Words
// alone are not enough: "run the tests" means one command in one
// checkout and another next door, so a recipe only fires where it was
// learned.
type Context struct {
	Cwd       string   `json:"cwd,omitempty"`   // working directory of the session
	Root      string   `json:"root,omitempty"`  // checkout holding Cwd, "" outside one
	Paths     []string `json:"paths,omitempty"` // paths the turn named: prompt, code, files written
	Repos     []string `json:"repos,omitempty"` // checkouts those paths live in, or the inherited focus
	Inherited bool     `json:"inherited,omitempty"`
	Prev      string   `json:"prev,omitempty"`  // the prompt before this one in the session
	Files     []string `json:"files,omitempty"` // files the turn wrote (the done entry)
}

// SameRepo is the firing gate: the two turns name a checkout in
// common (a session run from $HOME has no telling working directory;
// what it is about is in its paths).
func (c Context) SameRepo(o Context) bool {
	for _, r := range c.Repos {
		if slices.Contains(o.Repos, r) {
			return true
		}
	}
	return false
}

// SameDir is the stricter comparison: the exact working directory.
func (c Context) SameDir(o Context) bool { return c.Cwd != "" && c.Cwd == o.Cwd }

// focus is the checkout the turn is about, "" when it names none.
func (c Context) focus() string {
	if len(c.Repos) == 0 {
		return ""
	}
	return c.Repos[len(c.Repos)-1]
}

// where says which checkout(s) a turn was about, for the report.
func (c Context) where() string {
	if len(c.Repos) == 0 {
		return "in no checkout (" + shortCwd(c.Cwd) + ")"
	}
	s := "in " + strings.Join(shortAll(c.Repos), ", ")
	if c.Inherited {
		s += " (from the session)"
	}
	return s
}

// Match is the best recipe for a prompt and how sure the matcher is.
type Match struct {
	Recipe Recipe
	Score  float64
}

// Threshold is the score a match needs before the shadow log counts it
// as "would fire". Strict on purpose: a wrong fast path is worse than
// no fast path.
const Threshold = 0.85

var stop = map[string]bool{}

func init() {
	for _, w := range strings.Fields(`a an the and or of to in on at for with by from is are be it this that
		me my i you your please can could would just now then so up out into them these those do let`) {
		stop[w] = true
	}
}

// tokens lower-cases, splits on anything that is not a letter, digit,
// '/', '.', '_' or '-' (so paths and flags survive) and drops stop
// words.
func tokens(s string) []string {
	f := func(r rune) bool {
		return !(unicode.IsLetter(r) || unicode.IsDigit(r) || r == '/' || r == '.' || r == '_' || r == '-')
	}
	var out []string
	for _, w := range strings.FieldsFunc(strings.ToLower(s), f) {
		w = strings.Trim(w, ".-_")
		if w == "" || stop[w] {
			continue
		}
		out = append(out, w)
	}
	return out
}

// Index scores prompts against recipes with IDF-weighted, typo-tolerant
// bag-of-words overlap.
type Index struct {
	recipes []Recipe
	toks    [][]string
	df      map[string]int
}

// NewIndex builds the index; nil recipes give an index that never
// matches.
func NewIndex(recipes []Recipe) *Index {
	ix := &Index{df: map[string]int{}}
	for _, r := range recipes {
		ix.Add(r)
	}
	return ix
}

// Add appends one recipe.
func (ix *Index) Add(r Recipe) {
	t := tokens(r.Phrasing)
	ix.recipes = append(ix.recipes, r)
	ix.toks = append(ix.toks, t)
	seen := map[string]bool{}
	for _, w := range t {
		if !seen[w] {
			seen[w] = true
			ix.df[w]++
		}
	}
}

// Len is the recipe count.
func (ix *Index) Len() int { return len(ix.recipes) }

func (ix *Index) idf(w string) float64 {
	return math.Log(1+float64(len(ix.recipes))) - math.Log(float64(ix.df[w])) + 1
}

// Best returns the highest-scoring recipe for prompt among those
// accept admits (nil admits all), or ok=false when there is none or
// the prompt has no tokens. Score is symmetric-ish:
// the IDF-weighted share of recipe tokens found in the prompt, scaled
// by the share of prompt tokens found in the recipe (so a prompt that
// says a lot more than the recipe does not match it).
func (ix *Index) Best(prompt string, accept func(Recipe) bool) (m Match, ok bool) {
	q := tokens(prompt)
	if len(q) == 0 {
		return m, false
	}
	for i, rt := range ix.toks {
		if len(rt) == 0 || (accept != nil && !accept(ix.recipes[i])) {
			continue
		}
		var got, total float64
		usedQ := make([]bool, len(q))
		for _, w := range rt {
			wt := ix.idf(w)
			total += wt
			for j, qw := range q {
				if !usedQ[j] && similar(w, qw) {
					usedQ[j] = true
					got += wt
					break
				}
			}
		}
		if total == 0 {
			continue
		}
		matchedQ := 0
		for _, u := range usedQ {
			if u {
				matchedQ++
			}
		}
		s := (got / total) * (float64(matchedQ) / float64(len(q)))
		if s > m.Score {
			m = Match{Recipe: ix.recipes[i], Score: s}
			ok = true
		}
	}
	return m, ok
}

// similar is an exact match or a Levenshtein ratio of at least 0.8 on
// words of 4+ runes (short words have no room for a typo).
func similar(a, b string) bool {
	if a == b {
		return true
	}
	if len(a) < 4 || len(b) < 4 {
		return false
	}
	d := levenshtein(a, b)
	n := max(len([]rune(a)), len([]rune(b)))
	return 1-float64(d)/float64(n) >= 0.8
}

func levenshtein(a, b string) int {
	ra, rb := []rune(a), []rune(b)
	prev := make([]int, len(rb)+1)
	cur := make([]int, len(rb)+1)
	for j := range prev {
		prev[j] = j
	}
	for i := 1; i <= len(ra); i++ {
		cur[0] = i
		for j := 1; j <= len(rb); j++ {
			cost := 1
			if ra[i-1] == rb[j-1] {
				cost = 0
			}
			cur[j] = min(prev[j]+1, cur[j-1]+1, prev[j-1]+cost)
		}
		prev, cur = cur, prev
	}
	return prev[len(rb)]
}
