package wiki

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// benchWiki is a home the size a daily driver reaches: many long
// sessions, a few dozen pages each citing a handful of entries.
func benchWiki(b *testing.B) *Store {
	b.Helper()
	t := &testing.T{}
	home := b.TempDir()
	s := Open(home)
	at := time.Date(2026, 9, 1, 12, 0, 0, 0, time.UTC)
	for i := range 200 {
		es := make([]entry, 0, 1500)
		for n := range 1500 {
			kind := "result"
			if n%3 == 0 {
				kind = "assistant"
			}
			es = append(es, entry{Seq: int64(n + 2), Kind: kind, Data: map[string]any{"text": strings.Repeat("some tool output line ", 8)}})
		}
		writeSession(t, s.p.hist, fmt.Sprintf("s%d", i), "/repo", at, es)
	}
	var index strings.Builder
	index.WriteString("# Wiki index\n\n## bench\n\n")
	for p := range 30 {
		var body strings.Builder
		fmt.Fprintf(&body, "# Page %d\n\nA lede.\n\n## Facts\n\n", p)
		for c := range 20 {
			fmt.Fprintf(&body, "- A claim about tool output `s%d#%d`.\n", (p*7+c)%200, 10+c*3)
		}
		write(t, filepath.Join(s.p.wiki, "topics", "bench", fmt.Sprintf("p%d.md", p)), body.String())
		fmt.Fprintf(&index, "- [Page %d](topics/bench/p%d.md) — bench\n", p, p)
	}
	write(t, filepath.Join(s.p.wiki, "index.md"), index.String())
	return s
}

func BenchmarkIndex(b *testing.B) {
	s := benchWiki(b)
	b.ResetTimer()
	for b.Loop() {
		s.Index(time.Now())
	}
}
