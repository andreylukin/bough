package skills

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/unreal/prompt"
)

// engineParts is what the engine row builds from this row: the listing
// becomes the catalogue part of the frozen prompt.
func engineParts(s *Skills) prompt.Parts {
	var p prompt.Parts
	for _, l := range s.Listing() {
		p.Skills = append(p.Skills, prompt.Skill{Name: l.Name, Description: l.Description, Path: l.Path})
	}
	return p
}

// On the engine the same SKILL.md files serve both mechanisms: the
// catalogue names each one with the absolute path the model views, a
// mention still injects the body at Submit, and an off skill is in
// neither. A manual skill is listed but not injected by a mention.
func TestEngineCatalogueAndMentionInject(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pool := filepath.Join(home, ".bough", "skills")
	addSkill(t, pool, "restish", "---\ndescription: Use when calling\n  REST APIs from the shell.\n---\nrestish body")
	addSkill(t, pool, "parallel", "---\ndescription: \""+strings.Repeat("long ", 100)+"\"\nmanual: true\n---\nweb search body")
	addSkill(t, pool, "hidden", "---\ndescription: switched off\n---\nhidden body")
	writeOff(t, home, "skill:hidden")
	s := DefaultFor(home, home)

	list := s.Listing()
	if len(list) != 2 || list[0].Name != "parallel" || list[1].Name != "restish" {
		t.Fatalf("listing = %+v", list)
	}
	if want := filepath.Join(pool, "restish", "SKILL.md"); list[1].Path != want || !filepath.IsAbs(list[1].Path) {
		t.Fatalf("path = %q, want %q", list[1].Path, want)
	}
	if list[1].Description != "Use when calling" {
		t.Fatalf("description = %q", list[1].Description)
	}
	if r := []rune(list[0].Description); len(r) != maxListedDescription || !strings.HasSuffix(list[0].Description, "…") {
		t.Fatalf("long description not capped: %d runes", len(r))
	}

	sys := prompt.Compose(engineParts(s))
	if !strings.Contains(sys, "- restish: Use when calling ("+list[1].Path+")") || strings.Contains(sys, "hidden") {
		t.Fatalf("catalogue:\n%s", sys)
	}

	if got := s.Inject("use restish for this"); len(got) != 1 || !strings.Contains(got[0], "restish body") {
		t.Fatalf("mention inject = %v", got)
	}
	if got := s.Inject("do these in parallel"); len(got) != 0 {
		t.Fatalf("manual skill injected on a mention: %v", got)
	}
	if got := s.Inject("hidden restish"); len(got) != 1 || strings.Contains(got[0], "hidden body") {
		t.Fatalf("off skill injected: %v", got)
	}
}

// A skill added mid-session reaches the model as a reminder carrying
// the whole new catalogue, never as an edit to the frozen prompt.
func TestEngineNewSkillIsAReminder(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pool := filepath.Join(home, ".bough", "skills")
	addSkill(t, pool, "restish", "---\ndescription: REST\n---\nbody")
	s := DefaultFor(home, home)
	before := engineParts(s)
	if text, _ := prompt.Reminder(before, engineParts(s)); text != "" {
		t.Fatalf("no change reminded: %s", text)
	}
	addSkill(t, pool, "kubectl", "---\ndescription: clusters\n---\nbody")
	text, changed := prompt.Reminder(before, engineParts(s))
	if len(changed) != 1 || changed[0] != "skills" || !strings.Contains(text, "- kubectl: clusters") || !strings.Contains(text, "- restish: REST") {
		t.Fatalf("changed %v:\n%s", changed, text)
	}
}
