package pipeline

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

const criteria = "The export command must write a CSV file with one header row and preserve the column order of the source table."

func TestLeakFilter(t *testing.T) {
	h := filepath.Join(t.TempDir(), "a.md")
	if err := os.WriteFile(h, []byte("# Acceptance\n\n"+criteria+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	quote := "Failed: per the spec, the EXPORT command must write a CSV file, with one header row. VERDICT: FAIL"
	if hit, phrase := leaks(quote, []string{h}); !hit || phrase != "the export command must write a csv file" {
		t.Errorf("8-word quote: hit=%v phrase=%q", hit, phrase)
	}
	para := "Exporting produced no header line and the columns came out shuffled.\nVERDICT: FAIL"
	if hit, _ := leaks(para, []string{h}); hit {
		t.Error("paraphrase flagged as a leak")
	}
	short := "The export command must work. One header row. VERDICT: FAIL"
	if hit, _ := leaks(short, []string{h}); hit {
		t.Error("short common phrases flagged as a leak")
	}
}

func TestStageHoldout(t *testing.T) {
	p, err := writePipeline(t, holdoutYML, map[string]string{"acceptance/a.md": criteria})
	if err != nil {
		t.Fatal(err)
	}
	run := t.TempDir()
	staged, err := stageHoldout(p, run)
	if err != nil {
		t.Fatal(err)
	}
	if len(staged) != 1 || staged[0] != filepath.Join(run, "holdout", "a.md") {
		t.Fatalf("staged = %v", staged)
	}
	st, err := os.Stat(staged[0])
	if err != nil || st.Mode().Perm() != 0o600 {
		t.Errorf("staged mode = %v, %v", st, err)
	}
}

const holdoutYML = `
name: h
start: coder
nodes:
  coder:
    type: agent
    mode: project
    project: demo
    next: validator
    fail: coder
    max_visits: 2
  validator:
    type: agent
    holdout: [acceptance/*.md]
    verdict: true
    pass: done
    fail: coder
    max_visits: 2
`

func TestPreflightHoldout(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	home := t.TempDir()
	// A holdout file under ~/.bough/scratch is reachable from an orb.
	p, err := writePipeline(t, holdoutYML, map[string]string{"acceptance/a.md": criteria})
	if err != nil {
		t.Fatal(err)
	}
	scratch := filepath.Join(home, ".bough", "scratch", "s1")
	os.MkdirAll(scratch, 0o755)
	p.Dir = scratch
	os.WriteFile(filepath.Join(scratch, "a.md"), []byte(criteria), 0o644)
	p.Nodes["validator"].Holdout = []string{"a.md"}
	if err := preflightHoldout(p, []string{"x"}, home); err == nil || !strings.Contains(err.Error(), "scratch") {
		t.Errorf("scratch holdout err = %v", err)
	}

	// A holdout file tracked in the project's checkout is refused.
	repo := t.TempDir()
	for _, args := range [][]string{{"init", "-q"}, {"add", "."}} {
		os.WriteFile(filepath.Join(repo, "acc.md"), []byte(criteria), 0o644)
		if out, err := exec.Command("git", append([]string{"-C", repo}, args...)...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	writeProject(t, home, "demo", "repos:\n  - path: "+repo+"\n")
	p.Dir = repo
	p.Nodes["validator"].Holdout = []string{"acc.md"}
	if err := preflightHoldout(p, []string{"x"}, home); err == nil || !strings.Contains(err.Error(), "tracked") {
		t.Errorf("tracked holdout err = %v", err)
	}

	// Outside every checkout and shared dir it passes.
	outside := t.TempDir()
	os.WriteFile(filepath.Join(outside, "acc.md"), []byte(criteria), 0o644)
	p.Dir = outside
	if err := preflightHoldout(p, []string{"x"}, home); err != nil {
		t.Errorf("clean holdout err = %v", err)
	}
}

func writeProject(t *testing.T, home, slug, yml string) {
	t.Helper()
	dir := filepath.Join(home, ".bough", "projects", slug)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "project.yml"), []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
}
