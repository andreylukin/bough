package tracecheck

import (
	"encoding/json"
	"fmt"
	"reflect"
	"sort"
)

// Step is one abstract step of a trace: the action the system under test
// took and the state it observed afterwards. State may name only some of
// the spec's variables: fields it leaves out are not checked, so a trace
// can record just what the implementation exposes. A first step named
// "Init" checks the initial state instead of taking a link.
type Step struct {
	Action string         `json:"action"`
	State  map[string]any `json:"state,omitempty"`
}

// Violation is the first step the graph does not allow.
type Violation struct {
	Index   int      // position in the trace
	Step    Step     // the offending step
	Reason  string   // why it is not allowed
	Allowed []string // actions the graph enables before this step, sorted
}

func (v *Violation) Error() string {
	return fmt.Sprintf("step %d (%s): %s; allowed: %v", v.Index, v.Step.Action, v.Reason, v.Allowed)
}

// Check replays steps from the initial state and returns the first step
// the graph does not allow, or nil when the whole trace is a path in it.
//
// It tracks a set of candidate nodes rather than one: an action with a
// fork (`any`, `oneof`) reaches several states, and a partial Step.State
// may not tell them apart until a later step does.
func (g *Graph) Check(steps []Step) *Violation {
	cur := []int{0}
	for i, s := range steps {
		want, err := normalize(s.State)
		if err != nil {
			return &Violation{Index: i, Step: s, Reason: err.Error()}
		}
		if i == 0 && s.Action == "Init" {
			if !matches(g.Nodes[0].State, want) {
				return &Violation{Index: i, Step: s, Reason: fmt.Sprintf("initial state is %s, not %s", js(g.Nodes[0].State), js(want))}
			}
			continue
		}
		allowed := g.actions(cur)
		var next []int
		for _, n := range cur {
			for _, li := range g.out[n] {
				if l := g.Links[li]; l.Type == "action" && l.Name == s.Action {
					next = append(next, l.Dest)
				}
			}
		}
		if len(next) == 0 {
			return &Violation{Index: i, Step: s, Reason: fmt.Sprintf("action %q is not enabled", s.Action), Allowed: allowed}
		}
		next = g.settle(next)
		var kept []int
		for _, n := range next {
			if matches(g.Nodes[n].State, want) {
				kept = append(kept, n)
			}
		}
		if len(kept) == 0 {
			var reach []string
			for _, n := range next {
				reach = append(reach, js(g.Nodes[n].State))
			}
			return &Violation{Index: i, Step: s, Allowed: allowed,
				Reason: fmt.Sprintf("state %s after %q matches none of the reachable states %v", js(want), s.Action, reach)}
		}
		cur = kept
	}
	return nil
}

// actions lists the action names enabled from any candidate node.
func (g *Graph) actions(nodes []int) []string {
	seen := map[string]bool{}
	var out []string
	for _, n := range nodes {
		for _, li := range g.out[n] {
			if l := g.Links[li]; l.Type == "action" && !seen[l.Name] {
				seen[l.Name] = true
				out = append(out, l.Name)
			}
		}
	}
	sort.Strings(out)
	return out
}

// settle follows fork links (fizz writes them with no type, out of a node
// whose yield point is the action's name) until each path reaches a
// settled "yield" node: the intermediate node still carries the state from
// before the choice, so matching against it would be wrong. A non-yield
// node with no fork links out is kept as it is.
func (g *Graph) settle(nodes []int) []int {
	seen := map[int]bool{}
	var out []int
	var walk func(int)
	walk = func(n int) {
		if seen[n] {
			return
		}
		seen[n] = true
		forked := false
		if g.Nodes[n].Name != "yield" {
			for _, li := range g.out[n] {
				if l := g.Links[li]; l.Type != "action" {
					forked = true
					walk(l.Dest)
				}
			}
		}
		if !forked {
			out = append(out, n)
		}
	}
	for _, n := range nodes {
		walk(n)
	}
	sort.Ints(out)
	return out
}

// normalize round-trips a state through JSON so Go ints compare equal to
// the float64s decoded from the graph.
func normalize(s map[string]any) (map[string]any, error) {
	if len(s) == 0 {
		return nil, nil
	}
	b, err := json.Marshal(s)
	if err != nil {
		return nil, fmt.Errorf("state is not JSON: %w", err)
	}
	var out map[string]any
	err = json.Unmarshal(b, &out)
	return out, err
}

func matches(have, want map[string]any) bool {
	for k, v := range want {
		h, ok := have[k]
		if !ok || !reflect.DeepEqual(h, v) {
			return false
		}
	}
	return true
}

func js(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}
