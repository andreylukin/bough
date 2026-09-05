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
		m, ok := ix.Best(prompt)
		if !ok || m.Recipe.Code != want || m.Score < Threshold {
			t.Errorf("%q: got %+v ok=%v, want %s above threshold", prompt, m, ok, want)
		}
	}
	if m, ok := ix.Best("rewrite the parser in rust and add tests"); ok && m.Score >= Threshold {
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
		if r, ok := recipeOf(tr, "s", ""); ok {
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
	write("a", "/x", [2]string{"run the tests", "go test"})
	write("b", "/y", [2]string{"run tests", "go test"}, [2]string{"run the tets", "make check"})
	verdicts, ix, err := Replay(dir)
	if err != nil {
		t.Fatal(err)
	}
	if ix.Len() != 3 || len(verdicts) != 3 {
		t.Fatalf("want 3 recipes and 3 verdicts, got %d and %d", ix.Len(), len(verdicts))
	}
	if verdicts[0].Fire {
		t.Error("the first turn ever has nothing to match")
	}
	if !verdicts[1].Fire || !verdicts[1].SameCode {
		t.Errorf("second turn should fire and agree: %+v", verdicts[1])
	}
	if !verdicts[2].Fire || verdicts[2].SameCode {
		t.Errorf("third turn should fire and disagree: %+v", verdicts[2])
	}
	var b strings.Builder
	Report(&b, verdicts, ix, false)
	if !strings.Contains(b.String(), "3 recipes learned, 3 turns replayed, 2 would fire, 1 of those ran the same code") {
		t.Errorf("report tally:\n%s", b.String())
	}
}
