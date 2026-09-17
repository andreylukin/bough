package history

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A redactor set on the store rewrites every string in an entry, nested
// or not, before it is kept in memory or written to disk.
func TestAppendRedacts(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s.jsonl")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	s.SetRedact(func(v string) string { return strings.ReplaceAll(v, "sk-live-0123456789", "[redacted:K]") })
	data := map[string]any{"text": "a sk-live-0123456789", "out": []any{"x", map[string]any{"y": "sk-live-0123456789"}}, "argv": []string{"sk-live-0123456789"}, "rows": []map[string]any{{"z": "sk-live-0123456789"}}, "n": 3}
	e := s.Append("exec", data)
	if data["text"] != "a sk-live-0123456789" {
		t.Fatalf("caller's map mutated: %v", data)
	}
	s.Close()
	b, _ := os.ReadFile(path)
	if strings.Contains(string(b), "sk-live") || strings.Contains(e.Data["text"].(string), "sk-live") || !strings.Contains(string(b), "[redacted:K]") {
		t.Fatalf("entry %v\nfile %s", e.Data, b)
	}
	s.SetRedact(nil)
}
