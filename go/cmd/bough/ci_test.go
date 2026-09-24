package main

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/ci"
)

// ciMain end to end on a temp repo and a temp HOME: exit codes, the
// cached marker, the log subcommand, --no-wait and --json.
func TestCIMainExitCodesAndOutput(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	home, repo := t.TempDir(), t.TempDir()
	write := func(rel, body string) {
		p := filepath.Join(repo, rel)
		os.MkdirAll(filepath.Dir(p), 0o755)
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if out, err := exec.Command("git", "-C", repo, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	write("src/a.txt", "a\n")
	write(".bough/ci.yml", "checks:\n  unit:\n    run: grep -q good src/a.txt || { echo not-good-enough; exit 1; }\n    inputs: [\"src/**\"]\n")
	write("src/a.txt", "good\n")

	run := func(args ...string) (int, string, string) {
		var out, errb bytes.Buffer
		code := ciMain(context.Background(), args, &out, &errb, home, repo)
		return code, out.String(), errb.String()
	}
	if code, out, errs := run(); code != 0 || !strings.Contains(out, "unit") || !strings.Contains(out, "pass") || !strings.Contains(errs, "running unit") {
		t.Fatalf("first run: %d %q %q", code, out, errs)
	}
	if code, out, errs := run(); code != 0 || !strings.Contains(out, "cached from") || strings.Contains(errs, "running") {
		t.Fatalf("second run: %d %q %q", code, out, errs)
	}
	write("src/a.txt", "bad\n")
	if code, out, _ := run("--no-wait"); code != 2 || !strings.Contains(out, "unknown") {
		t.Fatalf("--no-wait after an edit: %d %q", code, out)
	}
	if code, out, _ := run(); code != 1 || !strings.Contains(out, "fail") || !strings.Contains(out, "bough ci log unit") {
		t.Fatalf("failing run: %d %q", code, out)
	}
	if code, out, _ := run("log", "unit"); code != 0 || !strings.Contains(out, "not-good-enough") {
		t.Fatalf("log: %d %q", code, out)
	}
	code, out, _ := run("--json")
	var rep ci.Report
	if err := json.Unmarshal([]byte(out), &rep); err != nil || code != 1 || len(rep.Checks) != 1 || rep.Checks[0].State != ci.StateFail || !rep.Checks[0].Cached {
		t.Fatalf("--json: %d %v %q", code, err, out)
	}
	if code, _, errs := run("--check", "nope"); code != 2 || !strings.Contains(errs, `no check "nope"`) {
		t.Fatalf("unknown check: %d %q", code, errs)
	}
	if code, _, _ := run("stray"); code != 2 {
		t.Fatalf("stray arg: %d", code)
	}
	if code, _, errs := run("log"); code != 2 || !strings.Contains(errs, "usage") {
		t.Fatalf("log without a check: %d %q", code, errs)
	}
}

func TestCIMainOutsideRepoIsUsageError(t *testing.T) {
	t.Parallel()
	var out, errb bytes.Buffer
	code := ciMain(context.Background(), nil, &out, &errb, t.TempDir(), t.TempDir())
	if code != 2 || !strings.Contains(errb.String(), "not in a git repository") {
		t.Fatalf("outside a repo: %d %q", code, errb.String())
	}
}
