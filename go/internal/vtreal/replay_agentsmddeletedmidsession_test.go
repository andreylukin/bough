package vtreal

// AGENTS.md deleted between turns, then recreated with different
// content. context-md reads the file fresh each turn, so each request's
// system prompt (recorded by the echo llm's SYSTEM! reply) must track
// the file on disk: old section present, then gone, then only the new.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const (
	agentsMdDeletedMidSessionOld = "# Old\n\nAGENTSMD_DELETED_OLD rule.\n"
	agentsMdDeletedMidSessionNew = "# New\n\nAGENTSMD_DELETED_NEW rule.\n"
)

// agentsMdDeletedMidSessionLabels counts "# Context: AGENTS.md" headers,
// i.e. the file blocks the context assembly put in this request.
func agentsMdDeletedMidSessionLabels(sys string) int {
	return strings.Count(sys, "# Context: AGENTS.md")
}

func TestAgentsMdDeletedMidSession(t *testing.T) {
	t.Parallel()
	a := agentsMdStart(t, 100, map[string]string{"AGENTS.md": agentsMdDeletedMidSessionOld})

	sys := agentsMdSystem(t, a, 1)
	if !strings.Contains(sys, "AGENTSMD_DELETED_OLD") || agentsMdDeletedMidSessionLabels(sys) != 1 {
		t.Fatalf("turn 1: old AGENTS.md not in the prompt exactly once:\n%s\n--- screen:\n%s", sys, a.text())
	}

	if err := os.Remove(filepath.Join(a.home, "AGENTS.md")); err != nil {
		t.Fatal(err)
	}
	sys = agentsMdSystem(t, a, 2)
	if strings.Contains(sys, "AGENTSMD_DELETED_OLD") || agentsMdDeletedMidSessionLabels(sys) != 0 {
		t.Errorf("turn 2: deleted AGENTS.md still in the prompt:\n%s\n--- screen:\n%s", sys, a.text())
	}
	a.check("after delete")

	agentsMdWrite(t, a, "AGENTS.md", agentsMdDeletedMidSessionNew)
	sys = agentsMdSystem(t, a, 3)
	if strings.Contains(sys, "AGENTSMD_DELETED_OLD") {
		t.Errorf("turn 3: stale old section resurfaced:\n%s\n--- screen:\n%s", sys, a.text())
	}
	if !strings.Contains(sys, "AGENTSMD_DELETED_NEW") || agentsMdDeletedMidSessionLabels(sys) != 1 {
		t.Errorf("turn 3: recreated AGENTS.md not in the prompt exactly once:\n%s\n--- screen:\n%s", sys, a.text())
	}
	a.check("after recreate")
}
