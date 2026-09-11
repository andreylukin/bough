package vtreal

// Assistant markdown on a real terminal: headings, lists, tables,
// non-js fences, inline code, links, blockquotes, rules and a very
// long reply, rendered by glamour through the loop and the TUI.
//
// Each test writes a one-turn tape (user input + the recorded reply)
// and replays it, so the reply text sits next to the assertions about
// it. Every failure prints the screen.

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

// markdownSizes are the pane widths the scenario asks for. 40 is where
// wrapping and table overflow bite; 120 is where nothing has to wrap.
var markdownSizes = []int{40, 80, 120}

const markdownRows = 44

// markdownTape writes a one-turn recording: the user asks, the model
// answers with markdown and runs nothing, which ends the turn.
func markdownTape(t *testing.T, input, reply string) string {
	t.Helper()
	entries := []map[string]any{
		{"seq": 1, "at": "2026-09-11T10:00:00Z", "kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"seq": 2, "at": "2026-09-11T10:00:01Z", "kind": "input", "data": map[string]any{"text": input}},
		{"seq": 3, "at": "2026-09-11T10:00:02Z", "kind": "assistant", "data": map[string]any{"text": reply}},
		{"seq": 4, "at": "2026-09-11T10:00:03Z", "kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for _, e := range entries {
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	path := filepath.Join(t.TempDir(), "markdown.jsonl")
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// markdownConfig is replayConfig with the model streaming one word
// every delayMs; at 0 the whole reply arrives as one burst of deltas.
func markdownConfig(tape string, delayMs int) string {
	return strings.Replace(replayConfig(tape), "config: {file:",
		fmt.Sprintf("config: {delay_ms: %d, file:", delayMs), 1) // the llm row comes first
}

// markdownRun replays one markdown reply at the given width and
// returns the settled app.
func markdownRun(t *testing.T, cols int, input, reply string) *app {
	t.Helper()
	return markdownRunPaced(t, cols, 0, input, reply)
}

func markdownRunPaced(t *testing.T, cols, delayMs int, input, reply string) *app {
	t.Helper()
	a := startCfg(t, cols, markdownRows, markdownConfig(markdownTape(t, input, reply), delayMs))
	a.check("boot")
	a.typeText(input)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	// A big reply is still being folded into the transcript when the
	// "done" entry lands; the status bar spins until it is. settled()
	// alone cannot see this — the spinner never stops changing.
	a.waitUntil(func(s string) bool { return !strings.ContainsAny(s, "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏") }, "the turn's spinner to stop")
	a.check("after reply")
	return a
}

// markdownEachSize runs body at every scenario width as its own subtest.
func markdownEachSize(t *testing.T, body func(t *testing.T, cols int)) {
	t.Helper()
	for _, cols := range markdownSizes {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			body(t, cols)
		})
	}
}

// markdownWants fails with the screen when a wanted string is missing.
func markdownWants(t *testing.T, a *app, want ...string) {
	t.Helper()
	s := a.text()
	for _, w := range want {
		if !strings.Contains(s, w) {
			t.Errorf("want %q on screen:\n%s", w, s)
		}
	}
}

// markdownNoRaw asserts nothing that should have been consumed by the
// renderer or the emulator is showing as text: escape introducers, SGR
// or OSC 8 remnants, fence markers, raw markdown punctuation.
func markdownNoRaw(t *testing.T, a *app) {
	t.Helper()
	s := a.text()
	for _, bad := range []string{"]8;;", "[0m", "[38;5;", "[39m", "```", "\u00a0"} {
		if strings.Contains(s, bad) {
			t.Errorf("raw %q visible as text:\n%s", bad, s)
		}
	}
	// Escapes must be consumed, not printed. Scanned per rune: the
	// screen is full of box drawing, whose UTF-8 continuation bytes
	// would trip a byte-level search for C1 codes.
	for _, r := range s {
		if r == '\n' {
			continue
		}
		if r < 0x20 || (r >= 0x7f && r <= 0x9f) {
			t.Errorf("control rune %U visible as text:\n%s", r, s)
			return
		}
	}
}

// TestMarkdownProse covers headings, inline code, a blockquote, a
// horizontal rule and a link in one reply.
func TestMarkdownProse(t *testing.T) {
	t.Parallel()
	const reply = `# Release notes

Fixed ` + "`a + b`" + ` in ` + "`main.go`" + `, so ` + "`go test`" + ` passes.

## Details

> The parser kept the old offset after a resize.

---

See [the release notes](https://example.com/notes) for the rest.

### Credits

Reported by a user.`
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "what changed?", reply)
		markdownWants(t, a,
			"Release notes", "Details", "Credits",
			"Fixed a + b in main.go", "go test",
			"The parser kept the old offset",
			"the release notes",
			"Reported by a user.",
		)
		// Padding artifacts glamour leaves around inline code.
		for _, bad := range []string{"Fixed  ", "main.go ,", "so  go"} {
			if strings.Contains(a.text(), bad) {
				t.Errorf("inline-code padding artifact %q:\n%s", bad, a.text())
			}
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownLink asserts the visible text of a link: glamour emits
// OSC 8, which the terminal must swallow, leaving the label (and the
// href, which glamour also prints) readable.
func TestMarkdownLink(t *testing.T) {
	t.Parallel()
	const reply = "Docs live at [the handbook](https://example.com/handbook) today."
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "where are the docs?", reply)
		markdownWants(t, a, "the handbook")
		// glamour prints the href next to the label; at 40 columns it
		// wraps, so only the host is a safe needle.
		if !strings.Contains(a.text(), "example.com") {
			t.Errorf("link target not readable on screen:\n%s", a.text())
		}
		if strings.Contains(a.text(), "](") {
			t.Errorf("raw markdown link syntax on screen:\n%s", a.text())
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownNestedLists checks that nesting survives rendering: the
// child item is indented further than its parent.
func TestMarkdownNestedLists(t *testing.T) {
	t.Parallel()
	const reply = `Plan:

1. Top one
   - Nested alpha
   - Nested beta
     - Deepest gamma
2. Top two
   - Nested delta

Done.`
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "outline it", reply)
		markdownWants(t, a, "Top one", "Nested alpha", "Nested beta", "Deepest gamma", "Top two", "Nested delta")
		indent := func(needle string) int {
			for _, l := range a.lines() {
				if strings.Contains(l, needle) {
					return len(l) - len(strings.TrimLeft(l, " "))
				}
			}
			return -1
		}
		top, nested, deepest := indent("Top one"), indent("Nested alpha"), indent("Deepest gamma")
		if top < 0 || nested <= top {
			t.Errorf("nested item not indented past its parent (top=%d nested=%d):\n%s", top, nested, a.text())
		}
		if deepest <= nested {
			t.Errorf("third level not indented past the second (nested=%d deepest=%d):\n%s", nested, deepest, a.text())
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownWideTable renders a table far wider than the pane. The
// pane invariant (no row wider than the terminal) is the check that
// matters: a wide row scrolls the whole transcript sideways.
func TestMarkdownWideTable(t *testing.T) {
	t.Parallel()
	const reply = `Here is the matrix.

| component | owner | status | latency budget | notes |
| --- | --- | --- | --- | --- |
| ingest pipeline | platform team | green | 250 milliseconds | backfill still running nightly |
| query planner | data team | amber | 900 milliseconds | rewrite landed behind a flag |
| web frontend | product team | green | 120 milliseconds | no open regressions this week |

That is everything.`
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "show the matrix", reply)
		// check() already asserted no row is wider than the pane.
		markdownWants(t, a, "That is everything.")
		if cols >= 120 {
			markdownWants(t, a, "ingest pipeline", "query planner")
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownForeignFences covers fenced code in the reply that is
// not js: python and bash are code the model is showing and stay,
// while an untagged or plaintext fence is a guessed result and the
// loop replaces it — neither may run.
func TestMarkdownForeignFences(t *testing.T) {
	t.Parallel()
	const reply = "Two ways to do it.\n\n" +
		"```python\nprint(\"hello from python\")\n```\n\n" +
		"Or from the shell:\n\n" +
		"```bash\nls -la /tmp/demo\n```\n\n" +
		"Either prints the same thing."
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "how do I print it?", reply)
		markdownWants(t, a, "hello from python", "ls -la /tmp/demo", "Either prints the same thing.")
		if strings.Contains(a.text(), "Ran ") {
			t.Errorf("a non-js fence was executed:\n%s", a.text())
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownGuessedOutputFence pins the other half of the fence
// rule: a plaintext fence is a model-guessed result and is replaced by
// a marker instead of rendering like real runtime output.
func TestMarkdownGuessedOutputFence(t *testing.T) {
	t.Parallel()
	const reply = "It prints:\n\n```plaintext\nhello from a fake run\n```\n\nThat is all."
	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRun(t, cols, "what does it print?", reply)
		markdownWants(t, a, "guessed output omitted", "That is all.")
		if strings.Contains(a.text(), "hello from a fake run") {
			t.Errorf("guessed output rendered as real output:\n%s", a.text())
		}
		markdownNoRaw(t, a)
	})
}

// TestMarkdownLongReply: 400 lines of markdown in one reply. The
// transcript must stay sound, the composer pinned, the tail visible,
// and a wheel trip up and back must not break either.
//
// Env-gated: paced at 5 ms a word (1 ms still overruns the ui buffer at
// this size, see markdownConfig) the reply takes ~17 s to stream.
//
//	BOUGH_MARKDOWN_LONG=1 go test ./internal/vtreal -run TestMarkdownLongReply
func TestMarkdownLongReply(t *testing.T) {
	if os.Getenv("BOUGH_MARKDOWN_LONG") == "" {
		t.Skip("set BOUGH_MARKDOWN_LONG=1 to replay the 400-line reply (~20 s)")
	}
	t.Parallel()
	var sb strings.Builder
	sb.WriteString("# Long report\n\n")
	for i := 1; i <= 400; i++ {
		switch i % 4 {
		case 0:
			fmt.Fprintf(&sb, "## Section %d\n\n", i)
		case 1:
			fmt.Fprintf(&sb, "- item %d with `code%d` inline\n", i, i)
		case 2:
			fmt.Fprintf(&sb, "> quoted line %d\n\n", i)
		default:
			fmt.Fprintf(&sb, "Paragraph line %d of the report.\n\n", i)
		}
	}
	sb.WriteString("\nThe very last line of the report.")
	reply := sb.String()

	markdownEachSize(t, func(t *testing.T, cols int) {
		a := markdownRunPaced(t, cols, 0, "write the long report", reply)
		markdownWants(t, a, "The very last line of the report.")
		ls := a.lines()
		if r := composerRow(ls); r < 0 || r < len(ls)-3 {
			t.Fatalf("composer not pinned after a 400-line reply (row %d of %d):\n%s", r, len(ls), a.text())
		}
		for range 60 {
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		}
		a.check("scrolled up")
		markdownNoRaw(t, a)
		for range 120 {
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelDown})
		}
		a.check("scrolled back")
		markdownWants(t, a, "The very last line of the report.")
		markdownNoRaw(t, a)
	})
}
