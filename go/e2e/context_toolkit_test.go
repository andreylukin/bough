//go:build !windows

package e2e

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A binary launched outside the checkout still discovers and invokes the
// bundled skill. Nothing is installed into HOME to make that happen.
func TestContextToolkitSkillShipsInBinary(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{sets: []string{"loop.plugin=engine-clm"}})
	b.send("/context-toolkit explain its byte offsets")
	b.waitFor("UTF-8 bytes")
	b.waitFor("expected_revision")
	if _, err := os.Stat(filepath.Join(b.home, ".bough", "skills", "context-toolkit")); !os.IsNotExist(err) {
		t.Fatalf("builtin unexpectedly installed HOME files: %v", err)
	}
	if strings.Contains(b.out.String(), "unknown command") {
		t.Fatalf("builtin command not registered:\n%s", b.out.String())
	}
}

// The real handler must honor the toolkit's own native hook names before
// decoding a mutation or consulting its context revision.
func TestContextToolkitMutationHooksRefuseBeforeInvocation(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"context_edit", "context_offload", "context_restore"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			script := tape(t,
				fmt.Sprintf(`{"want":"check hooks","calls":[{"id":"blocked","name":%q,"args":{}}]}`, name),
				`{"want":"TOOLKIT_HOOK_REFUSAL","text":"hook refusal received"}`,
			)
			b := launchEngine(t, script, launchOpts{
				sets: []string{"loop.plugin=engine-clm"},
				home: map[string]string{".bough/hooks/pre-code-exec/context.js": `if (event.tool.indexOf("context_") === 0) return {deny: "TOOLKIT_HOOK_REFUSAL"};`},
			})
			b.send("check hooks")
			out := b.finish()
			mustContain(t, out, "hook refusal received")
			calls := kinds(readEntries(t, onlySession(t, b.home)), "call")
			if len(calls) != 1 || !strings.Contains(fmt.Sprint(calls[0].Data), "blocked by hook: TOOLKIT_HOOK_REFUSAL") {
				t.Fatalf("mutation bypassed its native hook: %+v", calls)
			}
		})
	}
}
