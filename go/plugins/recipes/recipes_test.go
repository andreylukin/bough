package recipes

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

func TestTypoAndOrderTolerantMatch(t *testing.T) {
	ix := NewIndex([]Recipe{
		{Phrasing: "run the tests", Code: "A"},
		{Phrasing: "push it to main", Code: "B"},
		{Phrasing: "count the go files in plugins", Code: "C"},
	})
	for prompt, want := range map[string]string{
		"run tests":                     "A",
		"tests, run them":               "A",
		"push to main":                  "B",
		"cound the go files in plugins": "C",
	} {
		m, ok := ix.Best(prompt, nil)
		if !ok || m.Recipe.Code != want || m.Score < Threshold {
			t.Errorf("%q: got %+v ok=%v, want %s above threshold", prompt, m, ok, want)
		}
	}
	if m, ok := ix.Best("rewrite the parser in rust and add tests", nil); ok && m.Score >= Threshold {
		t.Errorf("a longer, different ask must not fire: %+v", m)
	}
}

func TestTurnsAndRecipes(t *testing.T) {
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "run tests"}},
		{Kind: "code", Data: map[string]any{"text": "tools.bash('go test')"}},
		{Kind: "done"},
		{Kind: "input", Data: map[string]any{"text": "fix it"}},
		{Kind: "code", Data: map[string]any{"text": "x"}},
		{Kind: "code", Data: map[string]any{"text": "y"}},
		{Kind: "done"},
		{Kind: "input", Data: map[string]any{"text": "again"}},
		{Kind: "code", Data: map[string]any{"text": "z"}},
		{Kind: "error", Data: map[string]any{"text": "boom"}},
		{Kind: "done"},
	}
	turns := Turns(entries)
	if len(turns) != 3 {
		t.Fatalf("want 3 turns, got %d", len(turns))
	}
	var got []string
	for _, tr := range turns {
		if r, ok := recipeOf(tr, "s", Context{}); ok {
			got = append(got, r.Phrasing)
		}
	}
	if len(got) != 1 || got[0] != "run tests" {
		t.Errorf("only the one-block clean turn is a recipe: %v", got)
	}
}

// Two clones under one home: a session run from $HOME names them in
// its code, and "run the tests" only matches the clone it is on.
func TestReplayGatesOnTheCheckoutNamed(t *testing.T) {
	home := t.TempDir()
	mk := func(rel string) string {
		p := filepath.Join(home, rel)
		if err := os.MkdirAll(filepath.Join(p, ".git"), 0o755); err != nil {
			t.Fatal(err)
		}
		return p
	}
	foo, bar := mk("repos/foo"), mk("repos/bar")
	dir := filepath.Join(home, "history")
	write := func(name, cwd string, turns ...[2]string) {
		st, err := history.Open(filepath.Join(dir, name+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		st.Append("meta", map[string]any{"cwd": cwd})
		for _, tr := range turns {
			st.Append("input", map[string]any{"text": tr[0]})
			st.Append("code", map[string]any{"text": tr[1]})
			st.Append("done", nil)
		}
		st.Close()
	}
	// Session a, from $HOME: works on foo, then on bar.
	write("a", home,
		[2]string{"run the tests", "tools.bash('cd " + foo + " && go test ./...')"},
		[2]string{"look at " + bar, "tools.bash('ls " + bar + "')"},
		[2]string{"run the tests", "tools.bash('cd " + bar + " && npm test')"},
	)
	// Session b, from $HOME: names bar, then asks with no path.
	write("b", home,
		[2]string{"what's in " + bar + "?", "tools.bash('ls " + bar + "')"},
		[2]string{"run the tests", "tools.bash('cd " + bar + " && npm test')"},
	)
	verdicts, ix, err := Replay(dir)
	if err != nil {
		t.Fatal(err)
	}
	if ix.Len() != 5 || len(verdicts) != 5 {
		t.Fatalf("want 5 recipes and 5 verdicts, got %d and %d", ix.Len(), len(verdicts))
	}
	// a3: focus moved to bar, foo's "run the tests" must not fire.
	if v := verdicts[2]; v.Repo.Fire || !v.Words.Fire || v.Ask.focus() != bar {
		t.Errorf("a3: gate must hold (focus %s): repo=%+v words=%+v", v.Ask.focus(), v.Repo, v.Words)
	}
	// b2: focus is bar (from b1), bar's recipe from session a fires
	// and is the same code.
	if v := verdicts[4]; !v.Repo.Fire || !v.Repo.SameCode || !v.Ask.Inherited || v.Ask.focus() != bar {
		t.Errorf("b2: should fire on bar's recipe: ask=%+v repo=%+v", v.Ask, v.Repo)
	}
	var b strings.Builder
	Report(&b, verdicts, ix, false)
	if !strings.Contains(b.String(), "same checkout (the gate)      1 would fire,   1 ran the same code") {
		t.Errorf("report:\n%s", b.String())
	}
}

func TestPaths(t *testing.T) {
	home, _ := os.UserHomeDir()
	got := Paths(`cd go && go test ./...; tools.readFile("/Users/a/repos/x/README.md"); ls ~/repos/y/; grep 300 lines ~400 http://x/y`, "/base")
	want := []string{"/Users/a/repos/x/README.md", filepath.Join(home, "repos/y"), "/base/go"}
	if strings.Join(got, " ") != strings.Join(want, " ") {
		t.Errorf("got %v\nwant %v", got, want)
	}
}

func TestReplayLearnsInOrder(t *testing.T) {
	dir := t.TempDir()
	write := func(name, cwd string, turns ...[2]string) {
		st, err := history.Open(filepath.Join(dir, name+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		st.Append("meta", map[string]any{"cwd": cwd})
		for _, tr := range turns {
			st.Append("input", map[string]any{"text": tr[0]})
			st.Append("code", map[string]any{"text": tr[1]})
			st.Append("done", nil)
		}
		st.Close()
	}
	x, y := filepath.Join(dir, "x"), filepath.Join(dir, "y")
	write("a", x, [2]string{"run the tests", "go test"})
	write("b", y, [2]string{"run tests", "make check"}, [2]string{"run the tets", "make check"})
	verdicts, ix, err := Replay(dir)
	if err != nil {
		t.Fatal(err)
	}
	if ix.Len() != 3 || len(verdicts) != 3 {
		t.Fatalf("want 3 recipes and 3 verdicts, got %d and %d", ix.Len(), len(verdicts))
	}
	if verdicts[0].Words.Fire {
		t.Error("the first turn ever has nothing to match")
	}
	// "run tests" in y: words match x's recipe (wrong command), but
	// the directory gate has nothing to fire on, and outside any
	// checkout the repo gate has nothing either.
	if v := verdicts[1]; v.Dir.Fire || v.Repo.Fire || !v.Words.Fire || v.Words.SameCode {
		t.Errorf("second turn: gates must hold, words alone would misfire: %+v", v)
	}
	// "run the tets" in y: the directory gate finds y's own recipe.
	if v := verdicts[2]; !v.Dir.Fire || !v.Dir.SameCode || v.Ctx.Prev != "run tests" {
		t.Errorf("third turn should fire in-directory and agree: %+v", v)
	}
	var b strings.Builder
	Report(&b, verdicts, ix, true)
	for _, want := range []string{"3 recipes learned, 3 turns replayed", "same directory                1 would fire,   1 ran the same code", "words only                    2 would fire,"} {
		if !strings.Contains(b.String(), want) {
			t.Errorf("report should say %q:\n%s", want, b.String())
		}
	}
}
