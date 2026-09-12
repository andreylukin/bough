package ccplugins

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// fakeHome builds a ~/.claude/plugins tree: a manifest listing the
// given entries, and an install directory per entry whose installPath
// is under the cache (an entry pointing outside it stays absent).
func fakeHome(t *testing.T, entries map[string][]map[string]string) string {
	t.Helper()
	home := t.TempDir()
	dir := filepath.Join(home, ".claude", "plugins")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	m := map[string]any{"version": 2, "plugins": entries}
	data, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "installed_plugins.json"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	return home
}

// install lays out skills/<skill>/SKILL.md and commands/<cmd>.md.
func install(t *testing.T, path string, skills, cmds []string) {
	t.Helper()
	for _, s := range skills {
		d := filepath.Join(path, "skills", s)
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(d, "SKILL.md"), []byte("---\ndescription: the "+s+" skill\n---\nbody"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	for _, c := range cmds {
		d := filepath.Join(path, "commands")
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(d, c+".md"), []byte("do "+c+" with $ARGUMENTS"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func TestLoadUserScope(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	path := filepath.Join(home, ".claude", "plugins", "cache", "uni-common")
	install(t, path, []string{"circleci"}, []string{"new-component"})
	writeManifest(t, home, map[string][]map[string]string{
		"uni-common@uni-claude-marketplace": {{"scope": "user", "installPath": path, "version": "0.6.0"}},
	})

	got, err := Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 {
		t.Fatalf("Load = %d plugins, want 1", len(got))
	}
	p := got[0]
	if p.Name != "uni-common" || p.Marketplace != "uni-claude-marketplace" || p.Version != "0.6.0" {
		t.Errorf("plugin = %+v", p)
	}
	if !p.Present || len(p.Skills) != 1 || p.Skills[0] != "circleci" || len(p.Commands) != 1 || p.Commands[0] != "new-component" {
		t.Errorf("plugin contents = %+v", p)
	}
	if dirs := SkillDirs(home, home); len(dirs) != 1 || dirs[0] != filepath.Join(path, "skills") {
		t.Errorf("SkillDirs = %v", dirs)
	}
}

func TestProjectScopeOnlyInsideProject(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	project := filepath.Join(home, "repos", "app")
	path := filepath.Join(home, ".claude", "plugins", "cache", "uni-frontend")
	install(t, path, []string{"storybook"}, nil)
	writeManifest(t, home, map[string][]map[string]string{
		"uni-frontend@uni-claude-marketplace": {{"scope": "project", "projectPath": project, "installPath": path, "version": "1.0.0"}},
	})

	if dirs := SkillDirs(home, home); len(dirs) != 0 {
		t.Errorf("SkillDirs from home = %v, want none", dirs)
	}
	if dirs := SkillDirs(home, filepath.Join(project, "src")); len(dirs) != 1 {
		t.Errorf("SkillDirs inside project = %v, want one", dirs)
	}
}

func TestMissingInstallPathReportedNotPresent(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	good := filepath.Join(home, ".claude", "plugins", "cache", "here")
	install(t, good, []string{"circleci"}, nil)
	gone := filepath.Join(home, ".claude", "plugins", "cache", "gone", "1.0.0")
	writeManifest(t, home, map[string][]map[string]string{
		"here@m": {{"scope": "user", "installPath": good, "version": "1"}},
		"gone@m": {{"scope": "user", "installPath": gone, "version": "1"}},
	})

	got, err := Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 {
		t.Fatalf("Load = %d plugins, want 2", len(got))
	}
	for _, p := range got {
		if p.ID == "gone@m" {
			if p.Present {
				t.Errorf("%s reported present", p.ID)
			}
			if p.Skills == nil || p.Commands == nil {
				t.Errorf("%s lists are nil, want empty", p.ID)
			}
		}
	}
	if dirs := SkillDirs(home, home); len(dirs) != 1 {
		t.Errorf("SkillDirs = %v, want just the present plugin", dirs)
	}
}

func TestOffPluginContributesNothing(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	path := filepath.Join(home, ".claude", "plugins", "cache", "uni-common")
	install(t, path, []string{"circleci"}, []string{"new-component"})
	writeManifest(t, home, map[string][]map[string]string{
		"uni-common@m": {{"scope": "user", "installPath": path, "version": "1"}},
	})
	writeOff(t, home, "plugin:uni-common@m")

	got, err := Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if !got[0].Off {
		t.Errorf("plugin not reported off: %+v", got[0])
	}
	if dirs := SkillDirs(home, home); len(dirs) != 0 {
		t.Errorf("SkillDirs = %v, want none", dirs)
	}
	if dirs := CommandDirs(home, home); len(dirs) != 0 {
		t.Errorf("CommandDirs = %v, want none", dirs)
	}
}

func TestMissingManifestIsQuiet(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	got, err := Load(home)
	if err != nil || len(got) != 0 {
		t.Fatalf("Load = (%v, %v), want (none, nil)", got, err)
	}
}

func TestMalformedManifestErrors(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	if err := os.WriteFile(Path(home), []byte("{not json"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Load(home); err == nil {
		t.Fatal("Load of a malformed manifest returned no error")
	}
}

func TestHooksMapKnownEventsAndReportTheRest(t *testing.T) {
	t.Parallel()
	home := fakeHome(t, nil)
	path := filepath.Join(home, ".claude", "plugins", "cache", "hooked")
	if err := os.MkdirAll(filepath.Join(path, "hooks"), 0o755); err != nil {
		t.Fatal(err)
	}
	body := `{"hooks":{
	  "SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}],
	  "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}],
	  "Notification":[{"hooks":[{"type":"command","command":"echo nope"}]}],
	  "PreCompact":[{"hooks":[{"type":"command","command":"echo nope"}]}]}}`
	if err := os.WriteFile(filepath.Join(path, "hooks", "hooks.json"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}

	hooks, ignored, err := Hooks(Plugin{ID: "hooked@m", InstallPath: path})
	if err != nil {
		t.Fatal(err)
	}
	if len(hooks) != 2 {
		t.Fatalf("hooks = %+v, want 2", hooks)
	}
	events := map[string]string{}
	for _, h := range hooks {
		events[h.CCEvent] = h.Event
	}
	if events["SessionStart"] != "session-start" || events["PreToolUse"] != "pre-code-exec" {
		t.Errorf("mapped events = %v", events)
	}
	if len(ignored) != 2 || ignored[0] != "Notification" || ignored[1] != "PreCompact" {
		t.Errorf("ignored = %v, want Notification and PreCompact", ignored)
	}
}

func writeManifest(t *testing.T, home string, entries map[string][]map[string]string) {
	t.Helper()
	data, err := json.Marshal(map[string]any{"version": 2, "plugins": entries})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(Path(home), data, 0o644); err != nil {
		t.Fatal(err)
	}
}

func writeOff(t *testing.T, home, entry string) {
	t.Helper()
	dir := filepath.Join(home, ".bough")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "off.yml"), []byte("off:\n  - "+entry+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
}
