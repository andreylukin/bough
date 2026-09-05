package recipes

import (
	"os"
	"path/filepath"
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

// Against the real history, when present: prints what the index would
// do, so a run with -v is the offline dry run.
func TestRealHistoryDryRun(t *testing.T) {
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, ".bough", "history")
	if _, err := os.Stat(dir); err != nil {
		t.Skip("no history")
	}
	ix := Load(dir, "")
	t.Logf("%d recipes", ix.Len())
	infos, _ := history.List(dir)
	fires := 0
	for _, info := range infos {
		entries, _ := history.Read(info.Path)
		for _, tr := range Turns(entries) {
			m, ok := ix.Best(tr.Input)
			if ok && m.Score >= Threshold && m.Recipe.Phrasing != tr.Input {
				fires++
				t.Logf("%.2f %q ↔ %q", m.Score, oneLine(tr.Input), oneLine(m.Recipe.Phrasing))
			}
		}
	}
	t.Logf("%d cross-turn fires", fires)
}
