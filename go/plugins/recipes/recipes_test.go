package recipes

import (
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
	// the directory gate has nothing to fire on.
	if v := verdicts[1]; v.Dir.Fire || !v.Words.Fire || v.Words.SameCode {
		t.Errorf("second turn: dir gate must hold, words alone would misfire: %+v", v)
	}
	// "run the tets" in y: the directory gate finds y's own recipe.
	if v := verdicts[2]; !v.Dir.Fire || !v.Dir.SameCode || v.Ctx.Prev != "run tests" {
		t.Errorf("third turn should fire in-directory and agree: %+v", v)
	}
	var b strings.Builder
	Report(&b, verdicts, ix, false)
	for _, want := range []string{"3 recipes learned, 3 turns replayed", "same directory (the gate)     1 would fire,   1 ran the same code", "words only                    2 would fire,"} {
		if !strings.Contains(b.String(), want) {
			t.Errorf("report should say %q:\n%s", want, b.String())
		}
	}
}
