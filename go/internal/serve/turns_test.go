package serve

import (
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

func TestTurnTests(t *testing.T) {
	entries := []history.Entry{
		{Kind: "code", Data: map[string]any{"text": "tools.bash('cd go && go test ./...')"}},
		{Kind: "result", Data: map[string]any{"exit": float64(1)}},
		{Kind: "done"},
		{Kind: "code", Data: map[string]any{"text": "ls"}},
		{Kind: "result", Data: map[string]any{"exit": float64(0)}},
		{Kind: "done"},
	}
	got := turnTests(entries)
	if got[1] == nil || got[1].Exit != 1 || got[1].Cmd != "go test" {
		t.Fatalf("turn 1 = %+v", got[1])
	}
	if got[2] != nil {
		t.Fatalf("turn 2 ran no tests, got %+v", got[2])
	}
}
