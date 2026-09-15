package wiki

import (
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestRedact(t *testing.T) {
	for _, tc := range []struct{ in, gone string }{
		{"token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N", "eyJhbGciOiJIUzI1NiJ9"},
		{"Authorization: Bearer abcdef0123456789xyz", "abcdef0123456789xyz"},
		{"git clone https://someone:hunter2secret@example.com/r.git", "hunter2secret"},
		{"export API_KEY=abc123def456", "abc123def456"},
		{`{"client_secret": "s3cr3tvalue"}`, "s3cr3tvalue"},
		{"DB_PASSWORD=correcthorse", "correcthorse"},
		{"key sk-abcdefghijklmnopqrstuvwx", "sk-abcdefghijklmnopqrstuvwx"},
	} {
		got := redact(tc.in)
		if strings.Contains(got, tc.gone) || !strings.Contains(got, "[redacted]") {
			t.Errorf("redact(%q) = %q", tc.in, got)
		}
	}
	for _, keep := range []string{"go test ./... passed", "https://example.com/path", "the tokenizer splits words"} {
		if got := redact(keep); got != keep {
			t.Errorf("redact(%q) = %q, want unchanged", keep, got)
		}
	}
}

func TestCiteExcerptAndSourceAreRedacted(t *testing.T) {
	home := t.TempDir()
	s := Open(home)
	at := time.Date(2026, 9, 10, 12, 0, 0, 0, time.UTC)
	writeSession(t, s.p.hist, "s1", "/repo", at, []entry{
		{Seq: 2, Kind: "result", Data: map[string]any{"text": "login ok\nexport SERVICE_TOKEN=abcd1234efgh5678"}},
	})
	write(t, filepath.Join(s.p.wiki, "topics", "t", "p.md"), "# P\n\n## Facts\n\n- The login uses SERVICE_TOKEN `s1#2` `s1#2`.\n")
	pg, err := s.Page("topics/t/p.md")
	if err != nil {
		t.Fatal(err)
	}
	var cites []Cite
	for _, b := range pg.Blocks {
		cites = append(cites, b.Cites...)
	}
	if len(cites) != 1 {
		t.Fatalf("duplicate citations kept: %+v", cites)
	}
	if strings.Contains(cites[0].Excerpt, "abcd1234efgh5678") {
		t.Fatalf("excerpt leaks: %q", cites[0].Excerpt)
	}
	src, err := s.Source("s1", 2)
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range src.Lines {
		if strings.Contains(l.Text, "abcd1234efgh5678") {
			t.Fatalf("source leaks: %q", l.Text)
		}
	}
}

func TestMalformedPageIsFlagged(t *testing.T) {
	bad := "1|# Ports\n2|\n27| 28| ## Facts\n29|- a claim\nUpdated: 2026-09-11 · Sessions:,,,,,\n"
	pg := parsePage("topics/t/bad.md", bad)
	if len(pg.Problems) != 2 {
		t.Fatalf("problems = %q", pg.Problems)
	}
	good := parsePage("topics/t/good.md", "# Ports\n\n- The table has a | pipe.\n- 3| is fine once.\n")
	if len(good.Problems) != 0 {
		t.Fatalf("a clean page was flagged: %q", good.Problems)
	}
}
