package projectdef

import (
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
)

// SetSecret edits project.yml the way SetName does: the shipped and
// hand-written comments, the key order and the name line all stay.
func TestSetSecretKeepsComments(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "c"); err != nil {
		t.Fatal(err)
	}
	if err := SetName(home, "c", "Cee"); err != nil {
		t.Fatal(err)
	}
	before, _ := ReadFile(home, "c", FileYAML)
	if err := WriteFile(home, "c", FileYAML, strings.Replace(before, Placeholder, home, 1)+"# my own note\nenv: {B: x}\n"); err != nil {
		t.Fatal(err)
	}
	for _, n := range []string{"A", "B", "A"} {
		if err := SetSecret(home, "c", n, "keychain:bough/c/"+n); err != nil {
			t.Fatal(err)
		}
	}
	got, _ := ReadFile(home, "c", FileYAML)
	for _, keep := range []string{"# bough project definition", "# caches: [/root/.cache/go-build]", "# env: {GOFLAGS: -mod=mod}", "# my own note", "\nname: Cee\n"} {
		if !strings.Contains(got, keep) {
			t.Fatalf("SetSecret dropped %q:\n%s", keep, got)
		}
	}
	if strings.Index(got, "name:") > strings.Index(got, "repos:") || strings.Index(got, "repos:") > strings.Index(got, "checks:") {
		t.Fatalf("key order moved:\n%s", got)
	}
	p, err := Load(home, "c")
	if err != nil {
		t.Fatal(err)
	}
	if len(p.Def.Secrets) != 2 || p.Def.Secrets["A"] != "keychain:bough/c/A" || p.Def.Secrets["B"] != "keychain:bough/c/B" || len(p.Def.Env) != 0 {
		t.Fatalf("def %+v\n%s", p.Def, got)
	}
}

// Writers of project.yml run in different processes (each child's
// SetSecret, serve's SetName and the editor's save); none may undo
// another's change.
func TestWritersDoNotLoseUpdates(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := CreateEmpty(home, "w", "Before"); err != nil {
		t.Fatal(err)
	}
	const n = 8
	var wg sync.WaitGroup
	errs := make(chan error, n+1)
	for i := range n {
		wg.Add(1)
		go func() {
			defer wg.Done()
			name := fmt.Sprintf("S%d", i)
			errs <- SetSecret(home, "w", name, "keychain:bough/w/"+name)
		}()
	}
	wg.Add(1)
	go func() { defer wg.Done(); errs <- SetName(home, "w", "After") }()
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	p, err := Load(home, "w")
	if err != nil {
		t.Fatal(err)
	}
	if len(p.Def.Secrets) != n || p.Def.Name != "After" {
		t.Fatalf("lost an update: name %q, secrets %v", p.Def.Name, p.Def.Secrets)
	}
}

// An editor save names the text it loaded; one whose base is no longer
// the file is refused and changes nothing.
func TestWriteFileIfRefusesAStaleBase(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := CreateEmpty(home, "e", "Alpha"); err != nil {
		t.Fatal(err)
	}
	loaded, _ := ReadFile(home, "e", FileYAML)
	if err := SetName(home, "e", "Beta"); err != nil {
		t.Fatal(err)
	}
	renamed, _ := ReadFile(home, "e", FileYAML)
	err := WriteFileIf(home, "e", FileYAML, loaded, loaded+"# mine\n")
	if !errors.Is(err, ErrStale) {
		t.Fatalf("stale save: err = %v", err)
	}
	if got, _ := ReadFile(home, "e", FileYAML); got != renamed {
		t.Fatalf("stale save reached disk:\n%s", got)
	}
	if err := WriteFileIf(home, "e", FileYAML, renamed, renamed+"# mine\n"); err != nil {
		t.Fatalf("current save: %v", err)
	}
	if got, _ := ReadFile(home, "e", FileYAML); got != renamed+"# mine\n" {
		t.Fatalf("current save not written:\n%s", got)
	}
}
