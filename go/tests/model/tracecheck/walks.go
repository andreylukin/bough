package tracecheck

// Walks is the Go twin of walks() in go/tests/web/model/graph.ts: a few
// long paths from the initial state instead of one path per target. The
// per-target paths the tests first used meant ~4,400 browser tests and
// ~34,000 steps, each path paying its own serve boot and replaying the same
// prefixes, and the suite no longer finished. A walk keeps going to the
// nearest thing it has not covered yet and starts over from Init when
// nothing is reachable or it has taken maxSteps actions.

// Cover picks what a set of walks must reach.
type Cover string

const (
	// CoverStates reaches every settled state at least once: the default,
	// cheap enough to gate every push.
	CoverStates Cover = "states"
	// CoverTransitions takes every link at least once: the exhaustive run.
	CoverTransitions Cover = "transitions"
)

// Walk is one generated path: the links it takes and the abstract trace
// they produce (fork links folded into the action that made them).
type Walk struct {
	Links []int  `json:"links"`
	Trace []Step `json:"trace"`
}

// Walks generates walks over g covering cover, each at most maxSteps
// actions long (0 means 50).
func (g *Graph) Walks(cover Cover, maxSteps int) []Walk {
	if maxSteps <= 0 {
		maxSteps = 50
	}
	reach := g.bfs([]int{0}, func(Link) bool { return true })
	want := map[int]bool{}
	if cover == CoverTransitions {
		for i, l := range g.Links {
			if _, ok := reach[l.Src]; ok {
				want[i] = true
			}
		}
	} else {
		for _, n := range g.Nodes {
			if _, ok := reach[n.Index]; ok && n.Name == "yield" && n.Index != 0 {
				want[n.Index] = true
			}
		}
	}
	take := func(links []int) {
		for _, i := range links {
			if cover == CoverTransitions {
				delete(want, i)
			} else {
				delete(want, g.Links[i].Dest)
			}
		}
	}
	var walks []Walk
	for stuck := 0; len(want) > 0 && stuck < 2; {
		cur, steps := 0, 0
		var links []int
		for steps < maxSteps {
			hit := g.nearest(cur, cover, want)
			if len(hit) == 0 {
				break
			}
			// A long jump to the next target is cheaper as the start of a
			// fresh walk (a new test, in parallel) than as this one's tail.
			cost := 0
			for _, i := range hit {
				if g.Links[i].Type == "action" {
					cost++
				}
			}
			if steps > 0 && steps+cost > maxSteps {
				break
			}
			links = append(links, hit...)
			take(hit)
			cur = g.Links[hit[len(hit)-1]].Dest
			steps += cost
		}
		tail := g.settleTail(cur)
		links = append(links, tail...)
		take(tail)
		if len(links) == 0 {
			stuck++
			continue
		}
		stuck = 0
		walks = append(walks, Walk{Links: links, Trace: g.trace(links)})
	}
	return walks
}

// nearest is the shortest link chain from cur to the closest target: a
// wanted state, or a node with a wanted link out of it (plus that link).
func (g *Graph) nearest(cur int, cover Cover, want map[int]bool) []int {
	parent, order := g.bfsOrder([]int{cur}, func(Link) bool { return true })
	for _, n := range order {
		if cover == CoverStates {
			if want[n] {
				return g.chain(parent, n)
			}
			continue
		}
		for _, i := range g.out[n] {
			if want[i] {
				return append(g.chain(parent, n), i)
			}
		}
	}
	return nil
}

// settleTail extends a walk that stops inside an action (an intermediate
// node) along fork links to the nearest settled state, which is the only
// kind an implementation can be observed in.
func (g *Graph) settleTail(n int) []int {
	if g.Nodes[n].Name == "yield" {
		return nil
	}
	parent, order := g.bfsOrder([]int{n}, func(l Link) bool { return l.Type != "action" })
	for _, m := range order {
		if g.Nodes[m].Name == "yield" {
			return g.chain(parent, m)
		}
	}
	return nil
}

func (g *Graph) trace(links []int) []Step {
	steps := []Step{{Action: "Init", State: g.Nodes[0].State}}
	for _, i := range links {
		l := g.Links[i]
		if l.Type == "action" {
			steps = append(steps, Step{Action: l.Name, State: g.Nodes[l.Dest].State})
		} else {
			steps[len(steps)-1].State = g.Nodes[l.Dest].State
		}
	}
	return steps
}

// chain is the link indices from the BFS root to n.
func (g *Graph) chain(parent map[int]int, n int) []int {
	var links []int
	for p := parent[n]; p >= 0; p = parent[g.Links[p].Src] {
		links = append([]int{p}, links...)
	}
	return links
}

// bfs maps every node reachable from roots to the link that first reached
// it (-1 for a root). Links are followed in file order, so the result is
// the same on every run and matches the TypeScript generator.
func (g *Graph) bfs(roots []int, follow func(Link) bool) map[int]int {
	parent, _ := g.bfsOrder(roots, follow)
	return parent
}

func (g *Graph) bfsOrder(roots []int, follow func(Link) bool) (map[int]int, []int) {
	parent := map[int]int{}
	order := append([]int(nil), roots...)
	for _, r := range roots {
		parent[r] = -1
	}
	for frontier := roots; len(frontier) > 0; {
		var next []int
		for _, n := range frontier {
			for _, i := range g.out[n] {
				l := g.Links[i]
				if _, seen := parent[l.Dest]; follow(l) && !seen {
					parent[l.Dest] = i
					next = append(next, l.Dest)
					order = append(order, l.Dest)
				}
			}
		}
		frontier = next
	}
	return parent, order
}
