// bough update / bough restart against the real binary. The sandbox
// HOME has no pidfile and no repos/bough, and env() blanks BOUGH_ROOT,
// so nothing here can touch a real checkout or session.
package e2e

import (
	"os"
	"testing"
)

func TestRestartNoPidfile(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{})
	out, code := runCLI(t, home, cwd, "restart")
	if code != 0 {
		t.Fatalf("exit code %d; output:\n%s", code, out)
	}
	mustContain(t, out, "no running web session; sessions pick up the new binary on next launch")
}

func TestUpdateOutsideCheckout(t *testing.T) {
	t.Parallel()
	if os.Getenv("BOUGH_BIN") != "" {
		t.Skip("BOUGH_BIN override may live inside a real checkout")
	}
	home, cwd, _ := sandbox(t, launchOpts{})
	out, code := runCLI(t, home, cwd, "update")
	if code == 0 {
		t.Fatalf("want nonzero exit; output:\n%s", out)
	}
	// The message is aimed at a binary install, which is how most
	// people have bough: it names the two ways to update one, and
	// keeps the search trail for a checkout that really was missed.
	mustContain(t, out,
		"not installed from a source checkout",
		"install.sh",
		"brew upgrade bough",
		"BOUGH_ROOT",
		"repos/bough")
}
