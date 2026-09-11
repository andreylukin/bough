package vtreal

// Huge block results replayed through the real binary: 500 lines, one
// 20k-char line, 5k lines. Each case checks the collapsed header's line
// count, that expanding stays inside the pane, that paging down reaches
// the reply below, and that every step settles within 5 s (the runaway
// / memory proxy; settle times are logged).

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const hugeOutputBudget = 5 * time.Second

// hugeOutputTape writes a one-turn tape whose single block returns result.
func hugeOutputTape(t *testing.T, result string) string {
	t.Helper()
	code := "console.log(tools.bash(\"gen\"))\n"
	entries := []struct {
		kind string
		data map[string]string
	}{
		{"meta", map[string]string{"cwd": "/tmp/demo"}},
		{"input", map[string]string{"text": "make output"}},
		{"assistant", map[string]string{"text": "```js\n" + code + "```"}},
		{"code", map[string]string{"text": code}},
		{"result", map[string]string{"code": code, "text": result}},
		{"assistant", map[string]string{"text": "All printed.\n\n```stop\nAll printed.\n```"}},
		{"done", map[string]string{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-10T10:00:00Z", "kind": e.kind, "data": e.data})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "huge.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func hugeOutputLines(n int) string {
	var sb strings.Builder
	for i := range n {
		fmt.Fprintf(&sb, "line %04d\n", i)
	}
	return sb.String()
}

// hugeOutputTimed runs step, waits for the screen to settle and fails
// past the budget.
func (a *app) hugeOutputTimed(what string, step func()) {
	a.t.Helper()
	t0 := time.Now()
	step()
	a.settled()
	d := time.Since(t0)
	a.t.Logf("%s settled in %v", what, d)
	if d > hugeOutputBudget {
		a.t.Errorf("%s took %v to settle (budget %v):\n%s", what, d, hugeOutputBudget, a.text())
	}
}

func hugeOutputRow(ls []string, sub string) int {
	for i, l := range ls {
		if strings.Contains(l, sub) {
			return i
		}
	}
	return -1
}

func TestHugeOutput(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		result string
		header string // collapsed header prefix, with the line count
		body   string // a substring of every body row
		pages  int    // pgdown presses enough to reach the bottom
	}{
		{"500Lines", hugeOutputLines(500), "▸ result (500 lines):", "line ", 40},
		{"20kCharLine", strings.Repeat("abcdefghij", 2000) + "\n", "▸ result (1 line):", "abcdefghij", 30},
		{"5kLines", hugeOutputLines(5000), "▸ result (5000 lines):", "line ", 280},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, 100, 30, replayConfig(hugeOutputTape(t, c.result)))
			a.typeText("make output")
			a.hugeOutputTimed("turn", func() {
				a.key(uv.KeyEnter, 0)
				if !a.waitDone(1, hugeOutputBudget) {
					t.Fatalf("turn not done within %v:\n%s", hugeOutputBudget, a.text())
				}
			})
			a.check("collapsed")

			ls := a.lines()
			row := hugeOutputRow(ls, c.header)
			if row < 0 {
				t.Fatalf("no collapsed header %q:\n%s", c.header, a.text())
			}
			for i, l := range ls {
				if i != row && strings.Contains(l, c.body) {
					t.Fatalf("collapsed block shows a body row at %d:\n%s", i, a.text())
				}
			}

			a.hugeOutputTimed("expand", func() {
				a.click(2, row)
				a.waitFor("▾ result")
			})
			a.check("expanded")
			s := a.text()
			if !strings.Contains(s, "▾ result") || !strings.Contains(s, c.body) {
				t.Fatalf("expanded block lost its header or body:\n%s", s)
			}
			if strings.Contains(s, "All printed.") {
				t.Fatalf("reply visible right after expanding a huge block (body not taller than the pane?):\n%s", s)
			}

			a.hugeOutputTimed("scroll", func() {
				for i := range c.pages {
					a.key(uv.KeyPgDown, 0)
					if i == c.pages/4 {
						a.check("mid-scroll")
						if !strings.Contains(a.text(), c.body) {
							t.Errorf("mid-scroll screen shows no body rows:\n%s", a.text())
						}
					}
				}
				a.waitFor("All printed.")
			})
			a.check("scrolled to bottom")
		})
	}
}
