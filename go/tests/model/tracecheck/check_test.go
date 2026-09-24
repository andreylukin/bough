package tracecheck

import (
	"encoding/json"
	"os"
	"reflect"
	"strings"
	"testing"
)

// The fixtures are real fizz v0.5.3 run dirs: light/ from
// go/tests/model/example/Light.fizz, door/ from testdata/Door.fizz,
// example/ from go/tests/model/specs/example.fizz.
// Regenerate with scripts/fizz.sh fizz --output-dir <dir> <spec>.

func load(t *testing.T, name string) *Graph {
	t.Helper()
	g, err := Load("../testdata/" + name)
	if err != nil {
		t.Fatal(err)
	}
	return g
}

func st(kv ...any) map[string]any {
	m := map[string]any{}
	for i := 0; i < len(kv); i += 2 {
		m[kv[i].(string)] = kv[i+1]
	}
	return m
}

func TestLoadLight(t *testing.T) {
	t.Parallel()
	g := load(t, "light")
	var states []string
	for _, n := range g.Nodes {
		states = append(states, n.State["color"].(string))
	}
	if want := []string{"red", "green", "yellow"}; !reflect.DeepEqual(states, want) {
		t.Fatalf("states = %v, want %v", states, want)
	}
	want := []Link{{0, 1, "Go", "action"}, {1, 2, "Slow", "action"}, {2, 0, "Stop", "action"}}
	if !reflect.DeepEqual(g.Links, want) {
		t.Fatalf("links = %+v, want %+v", g.Links, want)
	}
}

func TestLoadEmptyDir(t *testing.T) {
	t.Parallel()
	if _, err := Load(t.TempDir()); err == nil || !strings.Contains(err.Error(), "no nodes_") {
		t.Fatalf("err = %v, want a missing-shards error", err)
	}
}

func TestCheck(t *testing.T) {
	t.Parallel()
	light, door := load(t, "light"), load(t, "door")
	cases := []struct {
		name    string
		g       *Graph
		steps   []Step
		index   int // -1 = legal
		reason  string
		allowed []string
	}{
		{"light cycle twice", light, []Step{
			{"Init", st("color", "red")},
			{"Go", st("color", "green")}, {"Slow", st("color", "yellow")}, {"Stop", st("color", "red")},
			{"Go", st("color", "green")}, {"Slow", st("color", "yellow")}, {"Stop", st("color", "red")},
		}, -1, "", nil},
		{"light actions only", light, []Step{{Action: "Go"}, {Action: "Slow"}}, -1, "", nil},
		{"light skips yellow", light, []Step{
			{"Init", st("color", "red")}, {"Go", st("color", "green")}, {"Stop", st("color", "red")},
		}, 2, "not enabled", []string{"Slow"}},
		{"light wrong state", light, []Step{{"Go", st("color", "yellow")}}, 0, "state", []string{"Go"}},
		{"light wrong initial", light, []Step{{"Init", st("color", "green")}}, 0, "initial state", nil},
		{"light unknown field", light, []Step{{"Go", st("colour", "green")}}, 0, "state", []string{"Go"}},
		{"light unknown action", light, []Step{{Action: "Jump"}}, 0, "not enabled", []string{"Go"}},
		// Slam forks through an intermediate node; the abstract step
		// names only the action and the settled state after the choice.
		{"door slam locks", door, []Step{
			{"Open", st("door", "open")}, {"Slam", st("locked", true)}, {"Unlock", st("locked", false)},
		}, -1, "", nil},
		{"door slam unlocked", door, []Step{
			{"Open", nil}, {"Slam", st("door", "closed", "locked", false)}, {"Open", nil},
		}, -1, "", nil},
		// A partial state keeps both Slam outcomes alive; the next step
		// picks the one that allows it.
		{"door slam ambiguous", door, []Step{{"Open", nil}, {"Slam", st("door", "closed")}, {"Unlock", nil}}, -1, "", nil},
		{"door open while locked", door, []Step{
			{"Open", nil}, {"Slam", st("locked", true)}, {"Open", st("door", "open")},
		}, 2, "not enabled", []string{"Unlock"}},
		{"door slam into open", door, []Step{{"Open", nil}, {"Slam", st("door", "open")}}, 1, "state", []string{"Close", "Slam"}},
		{"door choice is not an action", door, []Step{{"Open", nil}, {Action: "Any:locked=True"}}, 1, "not enabled", []string{"Close", "Slam"}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			v := c.g.Check(c.steps)
			if c.index < 0 {
				if v != nil {
					t.Fatalf("legal trace rejected: %v", v)
				}
				return
			}
			if v == nil {
				t.Fatalf("illegal trace accepted, want a violation at step %d", c.index)
			}
			if v.Index != c.index || !strings.Contains(v.Reason, c.reason) || !reflect.DeepEqual(v.Allowed, c.allowed) {
				t.Fatalf("violation = %+v, want index %d, reason ~%q, allowed %v", v, c.index, c.reason, c.allowed)
			}
			if !strings.Contains(v.Error(), v.Step.Action) {
				t.Fatalf("Error() = %q does not name the action %q", v.Error(), v.Step.Action)
			}
		})
	}
}

// example/ is go/tests/model/specs/example.fizz, whose state lives in a
// role (fizzbee-mbt needs one). A role's fields read as
// "<Role>#<i>.<field>" and its actions as "<Role>#<i>.<Action>", the
// names the mbt server uses for both.
func TestCheckRoleState(t *testing.T) {
	t.Parallel()
	g := load(t, "example")
	legal := []Step{
		{"Init", st("Session#0.status", "idle", "Session#0.unseen", false)},
		{"Session#0.Prompt", st("Session#0.status", "running")},
		{"Session#0.Finish", st("Session#0.status", "done", "Session#0.unseen", true)},
		{"Session#0.View", st("Session#0.unseen", false, "Session#0.viewing", true)},
	}
	if v := g.Check(legal); v != nil {
		t.Fatalf("legal role trace rejected: %v", v)
	}
	unseenWhileViewing := []Step{
		{"Session#0.View", st("Session#0.viewing", true)},
		{"Session#0.Prompt", st("Session#0.status", "running")},
		{"Session#0.Finish", st("Session#0.unseen", true)},
	}
	if v := g.Check(unseenWhileViewing); v == nil || v.Index != 2 {
		t.Fatalf("unseen while viewing: violation = %v, want one at step 2", v)
	}
}

// A trace decoded from JSON (what the TS generator emits) carries
// float64 numbers; one built in Go carries ints. Both must match.
func TestCheckNumbersFromJSONOrGo(t *testing.T) {
	t.Parallel()
	g := &Graph{
		Nodes: []Node{{0, "yield", st("n", 0.0)}, {1, "yield", st("n", 1.0)}},
		Links: []Link{{0, 1, "Inc", "action"}},
	}
	g.index()
	if v := g.Check([]Step{{"Inc", st("n", 1)}}); v != nil {
		t.Fatalf("int state rejected: %v", v)
	}
	var steps []Step
	if err := json.Unmarshal([]byte(`[{"action":"Inc","state":{"n":1}}]`), &steps); err != nil {
		t.Fatal(err)
	}
	if v := g.Check(steps); v != nil {
		t.Fatalf("json state rejected: %v", v)
	}
	if v := g.Check([]Step{{"Inc", st("n", 2)}}); v == nil {
		t.Fatal("wrong number accepted")
	}
}

// testdata/door/paths.json is written by go/tests/web/model/gen.ts (its
// test fails when the file drifts). Every trace it emits must be legal
// here, and breaking one must be caught at the step that was broken.
func TestGeneratedPathsReplay(t *testing.T) {
	t.Parallel()
	g := load(t, "door")
	b, err := os.ReadFile("../testdata/door/paths.json")
	if err != nil {
		t.Fatal(err)
	}
	var out struct {
		Paths []struct {
			Trace []Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	if len(out.Paths) == 0 {
		t.Fatal("paths.json has no paths")
	}
	for i, p := range out.Paths {
		if v := g.Check(p.Trace); v != nil {
			t.Fatalf("generated path %d rejected: %v", i, v)
		}
		bad := append([]Step(nil), p.Trace...)
		last := len(bad) - 1
		bad[last].Action = bad[1].Action + "Twice" // no such action
		if v := g.Check(bad); v == nil || v.Index != last {
			t.Fatalf("path %d with a broken last step: violation %v, want at step %d", i, v, last)
		}
	}
}
