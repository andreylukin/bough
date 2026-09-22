package unreal

import (
	"os"
	"path/filepath"
	"regexp"
	"runtime/debug"
	"strings"
	"testing"

	// Linked only so the test binary's build info lists the harness.
	_ "github.com/unreallabsai/unreal-agent/harness/llm"
)

// The binary links exactly the pinned harness. A `go get` that moves
// the version without Pin (which the engine entry records) fails here.
func TestPinnedHarnessVersion(t *testing.T) {
	t.Parallel()
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		t.Fatal("no build info in the test binary")
	}
	var found *debug.Module
	for _, m := range bi.Deps {
		if m.Path == Module {
			found = m
		}
	}
	if found == nil {
		t.Fatalf("%s is not linked into the test binary", Module)
	}
	if found.Replace != nil {
		// §16: a local-path replace is never committed; a published fork
		// needs its reason in go.mod and a design review, not a quiet swap.
		t.Fatalf("%s is replaced by %s %s; the pin must be the upstream module", Module, found.Replace.Path, found.Replace.Version)
	}
	if found.Version != Pin {
		t.Fatalf("%s linked at %s, Pin says %s: bump Pin and PinSHA with go.mod", Module, found.Version, Pin)
	}
}

// go.mod names the SHA beside the version, so a reader of go.mod sees
// the commit the engine entries carry, and go.sum holds the hash that
// makes the pin real.
func TestGoModNamesPinSHA(t *testing.T) {
	t.Parallel()
	root := filepath.Join("..", "..")
	mod, err := os.ReadFile(filepath.Join(root, "go.mod"))
	if err != nil {
		t.Fatal(err)
	}
	line := regexp.MustCompile(`(?m)^\s*` + regexp.QuoteMeta(Module) + `\s+(\S+)(.*)$`).FindSubmatch(mod)
	if line == nil {
		t.Fatalf("go.mod has no require line for %s", Module)
	}
	if v := string(line[1]); v != Pin {
		t.Fatalf("go.mod requires %s %s, Pin is %s", Module, v, Pin)
	}
	if !strings.Contains(string(line[2]), PinSHA) {
		t.Fatalf("go.mod's %s line does not name PinSHA %s: %q", Module, PinSHA, line[0])
	}
	sum, err := os.ReadFile(filepath.Join(root, "go.sum"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(sum), Module+" "+Pin+" h1:") {
		t.Fatalf("go.sum has no h1 hash for %s %s", Module, Pin)
	}
}
