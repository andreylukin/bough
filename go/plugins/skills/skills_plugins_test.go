package skills

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"testing"
)

// fakePlugin writes a manifest entry and, unless absent, the install
// tree it points at: skills/<name>/SKILL.md.
func fakePlugin(t *testing.T, home, id, scope, projectPath string, skillNames []string, absent bool) {
	t.Helper()
	dir := filepath.Join(home, ".claude", "plugins")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	name, _, _ := splitID(id)
	installPath := filepath.Join(dir, "cache", name, "1.0.0")
	if !absent {
		for _, s := range skillNames {
			addSkill(t, filepath.Join(installPath, "skills"), s, "body of "+s)
		}
	}

	manifestPath := filepath.Join(dir, "installed_plugins.json")
	m := struct {
		Version int                         `json:"version"`
		Plugins map[string][]map[string]any `json:"plugins"`
	}{Version: 2, Plugins: map[string][]map[string]any{}}
	if data, err := os.ReadFile(manifestPath); err == nil {
		if err := json.Unmarshal(data, &m); err != nil {
			t.Fatal(err)
		}
	}
	entry := map[string]any{"scope": scope, "installPath": installPath, "version": "1.0.0"}
	if projectPath != "" {
		entry["projectPath"] = projectPath
	}
	m.Plugins[id] = []map[string]any{entry}
	data, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(manifestPath, data, 0o644); err != nil {
		t.Fatal(err)
	}
}

func splitID(id string) (name, market string, ok bool) {
	for i := range id {
		if id[i] == '@' {
			return id[:i], id[i+1:], true
		}
	}
	return id, "", false
}

func writeOff(t *testing.T, home string, entries ...string) {
	t.Helper()
	dir := filepath.Join(home, ".bough")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	body := "off:\n"
	for _, e := range entries {
		body += "  - " + e + "\n"
	}
	if err := os.WriteFile(filepath.Join(dir, "off.yml"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestPluginSkillJoinsPool(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	fakePlugin(t, home, "uni-common@m", "user", "", []string{"circleci"}, false)
	addSkill(t, filepath.Join(home, ".bough", "skills"), "restish", "restish body")

	s := DefaultFor(home, home)
	if names := s.Names(); !slices.Contains(names, "circleci") || !slices.Contains(names, "restish") {
		t.Fatalf("Names = %v, want both the plugin and the pool skill", names)
	}
	for _, info := range s.Catalog() {
		if info.Name == "circleci" && info.Source != "plugin" {
			t.Errorf("circleci source = %q, want plugin", info.Source)
		}
		if info.Name == "restish" && info.Source != "pool" {
			t.Errorf("restish source = %q, want pool", info.Source)
		}
	}
	if got := s.Inject("check circleci please"); len(got) != 1 {
		t.Fatalf("Inject = %d blocks, want 1", len(got))
	}
}

func TestProjectScopePluginOnlyInsideProject(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	project := filepath.Join(home, "repos", "app")
	fakePlugin(t, home, "uni-frontend@m", "project", project, []string{"storybook"}, false)

	if names := DefaultFor(home, home).Names(); slices.Contains(names, "storybook") {
		t.Errorf("Names from home = %v, want no project-scoped skill", names)
	}
	if names := DefaultFor(home, filepath.Join(project, "src")).Names(); !slices.Contains(names, "storybook") {
		t.Errorf("Names inside project = %v, want storybook", names)
	}
}

func TestMissingInstallPathDoesNotBreakTheScan(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	fakePlugin(t, home, "gone@m", "user", "", []string{"vanished"}, true)
	fakePlugin(t, home, "here@m", "user", "", []string{"circleci"}, false)

	names := DefaultFor(home, home).Names()
	if !slices.Contains(names, "circleci") || slices.Contains(names, "vanished") {
		t.Fatalf("Names = %v, want circleci only", names)
	}
}

func TestOffPluginContributesNoSkills(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	fakePlugin(t, home, "uni-common@m", "user", "", []string{"circleci"}, false)
	writeOff(t, home, "plugin:uni-common@m")

	s := DefaultFor(home, home)
	if names := s.Names(); slices.Contains(names, "circleci") {
		t.Fatalf("Names = %v, want nothing from the off plugin", names)
	}
	if got := s.Inject("check circleci please"); len(got) != 0 {
		t.Fatalf("Inject = %v, want nothing", got)
	}
}

func TestOffSkillIsNotInjectedButStillListed(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	addSkill(t, filepath.Join(home, ".bough", "skills"), "restish", "restish body")
	writeOff(t, home, "skill:restish")

	s := DefaultFor(home, home)
	if got := s.Inject("use restish"); len(got) != 0 {
		t.Fatalf("Inject = %v, want nothing", got)
	}
	if names := s.Names(); slices.Contains(names, "restish") {
		t.Errorf("Names = %v, want no off skill", names)
	}
	var found bool
	for _, info := range s.Catalog() {
		if info.Name == "restish" {
			found = true
			if !info.Off {
				t.Errorf("catalog entry not marked off: %+v", info)
			}
		}
	}
	if !found {
		t.Error("off skill missing from the catalog; the picker cannot switch it back on")
	}
}

// A plugin skill named after an ordinary word must go through the same
// guard a pool skill does: prose mentioning it injects nothing.
func TestPluginSkillHonoursTheCommonWordGuard(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	fakePlugin(t, home, "uni-common@m", "user", "", []string{"review", "manualish"}, false)
	install := filepath.Join(home, ".claude", "plugins", "cache", "uni-common", "1.0.0", "skills")
	addSkill(t, install, "manualish", "---\nmanual: true\n---\nbody")

	s := DefaultFor(home, home)
	if got := s.Inject("please review this diff and be manualish"); len(got) != 0 {
		t.Fatalf("Inject = %v, want nothing", got)
	}
	if got := s.Inject("/review this diff"); len(got) != 1 {
		t.Fatalf("Inject of an explicit /review = %d blocks, want 1", len(got))
	}
}
