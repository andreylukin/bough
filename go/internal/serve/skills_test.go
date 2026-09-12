package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

// seedSkill writes a SKILL.md pool entry under the fixture's HOME.
func seedSkill(t *testing.T, home, name, front string) {
	t.Helper()
	dir := filepath.Join(home, ".claude", "skills", name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte(front), 0o644); err != nil {
		t.Fatal(err)
	}
}

// The composer offers what the agent would actually find, so the
// picker and the TUI's slash palette never disagree.
func TestSkillCatalogue(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home // look at the fixture's pool, not the developer's
	seedSkill(t, f.home, "grill-me", "---\ndescription: Interrogate a design.\n---\n")
	seedSkill(t, f.home, "quiet", "---\ndescription: A manual one.\nmanual: true\n---\n")

	code, body := f.do(t, "GET", "/api/skills", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/skills = %d", code)
	}
	list, _ := body["skills"].([]any)
	if len(list) != 2 {
		t.Fatalf("skills = %d, want 2 (%v)", len(list), body["skills"])
	}
	got := map[string]map[string]any{}
	for _, it := range list {
		m, _ := it.(map[string]any)
		name, _ := m["name"].(string)
		got[name] = m
	}
	if _, ok := got["grill-me"]; !ok {
		t.Fatalf("grill-me missing from %v", got)
	}
	if s, _ := got["grill-me"]["summary"].(string); s == "" {
		t.Error("no summary: the picker is a list of bare names")
	}
	if m, _ := got["quiet"]["manual"].(bool); !m {
		t.Error("manual: true not reported")
	}
}

// An empty pool must still answer with a list, not a JSON null: the UI
// maps over it.
func TestSkillCatalogueEmpty(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	code, body := f.do(t, "GET", "/api/skills", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/skills = %d", code)
	}
	if _, ok := body["skills"].([]any); !ok {
		t.Fatalf("skills is not a list: %#v", body["skills"])
	}
}
