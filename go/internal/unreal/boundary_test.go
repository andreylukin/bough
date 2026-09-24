package unreal

import (
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

const self = "github.com/andreylukin/bough/"

// harnessImporters are the only packages that may import the harness or
// internal/unreal/... directly (go/docs/unreal-engine.md §16). Everyone
// else reaches the engine through service keys with plain types, so a
// harness bump is contained to these and never ripples into ui, serve
// or the tool rows.
var harnessImporters = []string{
	"internal/unreal",
	"internal/messagesapi",
	"internal/agentllm",
	"plugins/engine",
	"plugins/llm",
}

// testImporters may import the engine from their tests only: each
// checks that its row's text or hooks reach the engine as the loop sees
// them. A harness bump can break these tests too, which is why they are
// named here rather than left to be found by it.
var testImporters = []string{
	"e2e",
	"plugins/contextmd",
	"plugins/hooks",
	"plugins/rules",
	"plugins/skills",
	// engine_cancel_mid_call walks the spec against an in-process
	// Runtime: serve cannot reach that flow (its Stop exits the child).
	"tests/model/mbt",
}

// vocabularyPlugins are the plugin packages internal/unreal/... may
// import, for their types only (history.Entry, llm.Usage, loop's
// UsageDelta and DefaultProject): services arrive through session.Deps,
// never by importing the row that provides them (§3).
var vocabularyPlugins = []string{"plugins/history", "plugins/llm", "plugins/loop"}

func under(pkg, prefix string) bool {
	return pkg == self+prefix || strings.HasPrefix(pkg, self+prefix+"/")
}

func harness(imp string) bool {
	return imp == Module || strings.HasPrefix(imp, Module+"/") || under(imp, "internal/unreal")
}

func TestImportBoundary(t *testing.T) {
	t.Parallel()
	cmd := exec.Command("go", "list", "-f", `{{.ImportPath}}|{{join .Imports " "}}|{{join .TestImports " "}} {{join .XTestImports " "}}`, "./...")
	cmd.Dir = filepath.Join("..", "..")
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("go list: %v\n%s", err, out)
	}
	for line := range strings.SplitSeq(strings.TrimSpace(string(out)), "\n") {
		parts := strings.SplitN(line, "|", 3)
		if len(parts) != 3 {
			continue
		}
		pkg, imports, tests := parts[0], strings.Fields(parts[1]), strings.Fields(parts[2])
		allowed := slices.ContainsFunc(harnessImporters, func(p string) bool { return under(pkg, p) })
		for _, imp := range imports {
			if harness(imp) && !allowed {
				t.Errorf("%s imports %s; only %v may (go/docs/unreal-engine.md §16)", pkg, imp, harnessImporters)
			}
			if under(pkg, "internal/unreal") && under(imp, "plugins") &&
				!slices.ContainsFunc(vocabularyPlugins, func(p string) bool { return under(imp, p) }) {
				t.Errorf("%s imports %s; internal/unreal/... may import only %v, and reaches services through session.Deps", pkg, imp, vocabularyPlugins)
			}
		}
		testAllowed := allowed || slices.ContainsFunc(testImporters, func(p string) bool { return under(pkg, p) })
		for _, imp := range tests {
			if harness(imp) && !testAllowed {
				t.Errorf("%s's tests import %s; only %v and, in tests, %v may", pkg, imp, harnessImporters, testImporters)
			}
		}
	}
}
