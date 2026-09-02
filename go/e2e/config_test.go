// Run-from-anywhere: no bough.yml in cwd or HOME — the embedded
// default config mounts and the binary still works.
package e2e

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func emptySandbox(t *testing.T) (home, cwd string) {
	t.Helper()
	base := t.TempDir()
	home = filepath.Join(base, "home")
	cwd = filepath.Join(base, "cwd")
	for _, d := range []string{home, cwd} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	return home, cwd
}

func TestEmbeddedConfigHeadlessTurn(t *testing.T) {
	t.Parallel()
	home, cwd := emptySandbox(t)

	cmd := exec.Command(boughBin, "--set", "llm.plugin=llm-echo", "--headless")
	cmd.Dir = cwd
	cmd.Env = env(home)
	cmd.Stdin = strings.NewReader("hello from nowhere\n")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("run: %v\noutput:\n%s", err, out)
	}
	mustContain(t, string(out),
		"[assistant] echo: hello from nowhere",
		"[done]",
	)
	mustNotContain(t, string(out), "bough: using") // headless stderr stays quiet
}

func TestEmbeddedConfigRows(t *testing.T) {
	t.Parallel()
	home, cwd := emptySandbox(t)

	out, code := runCLI(t, home, cwd, "rows")
	if code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, out)
	}
	mustContain(t, out, "llm", "ui")
	mustNotContain(t, out, "bough: using") // a subcommand's stderr stays quiet

	out, code = runCLI(t, home, cwd, "rows", "--verbose")
	if code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, out)
	}
	mustContain(t, out, "bough: using embedded default config")
}
