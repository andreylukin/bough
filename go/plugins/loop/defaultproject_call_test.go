package loop

import (
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// An engine session resumed on the loop reads its native calls' output
// the way a result row reads, while a loop session's own call rows
// (tools/calls.go, no hseq) stay out: its result row already carries
// the block's output, and projecting both would say it twice.
func TestDefaultProjectFoldsEngineCallsOnly(t *testing.T) {
	t.Parallel()
	es := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "run it"}},
		{Kind: "call", Data: map[string]any{"text": "go test ./...", "tool": "bash", "output": "ok  pkg", "exit": 0, "hseq": 4}},
		{Kind: "call", Data: map[string]any{"text": "a.go", "tool": "view", "error": "no such file", "hseq": 5}},
		{Kind: "assistant", Data: map[string]any{"text": "done"}},
		{Kind: "code", Data: map[string]any{"text": "await tools.bash('ls')"}},
		{Kind: "call", Data: map[string]any{"text": "ls", "tool": "bash"}},
		{Kind: "result", Data: map[string]any{"text": "a.go"}},
	}
	got := DefaultProject(es)
	want := []string{
		"run it",
		toolOutputPrefix + "[bash: go test ./...]\nok  pkg",
		toolOutputPrefix + "[view: a.go]\nError: no such file",
		"done",
		toolOutputPrefix + "a.go",
	}
	if len(got) != len(want) {
		t.Fatalf("got %d messages, want %d: %+v", len(got), len(want), got)
	}
	for i := range want {
		if got[i].Content != want[i] {
			t.Errorf("message %d = %q, want %q", i, got[i].Content, want[i])
		}
	}
}
