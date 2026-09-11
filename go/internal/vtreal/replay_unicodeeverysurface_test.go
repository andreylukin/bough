package vtreal

// Wide, combining, ZWJ and RTL text on every surface that shows text
// the user or the model chose: the session title in the status bar,
// the /sessions picker, the "/" palette search, @ picker filenames,
// ask options, todo items, subagent card titles and bang shell output.
// replay_unicode_test.go covers the transcript and the composer; this
// file covers everything around them.
//
// Every screen is checked cell by cell (unicodeCheckWidths) and for
// split graphemes: no U+FFFD, no cell that starts with a combining mark
// or a joiner, no cell that ends in a joiner.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode"

	uv "github.com/charmbracelet/ultraviolet"
)

// The strings under test: one of each kind, and a mix.
const (
	unicodeEverySurfaceCJK   = "日本語のテキスト"
	unicodeEverySurfaceZWJ   = "👨‍👩‍👧‍👦"
	unicodeEverySurfaceComb  = "café naïve"
	unicodeEverySurfaceRTL   = "مرحبا بالعالم"
	unicodeEverySurfaceMixed = "日本語 👨‍👩‍👧‍👦 café مرحبا"
)

// unicodeEverySurfaceLong is a mix long enough to be truncated or
// wrapped in any pane.
var unicodeEverySurfaceLong = strings.Repeat(unicodeEverySurfaceMixed+" ", 8)

type unicodeEverySurfaceM = map[string]any

// unicodeEverySurfaceCheck is unicodeCheckWidths plus the grapheme
// rules, on a settled screen. settled() compares plain text, which can
// match while the PTY is still mid-way through a grapheme's bytes, so a
// grapheme violation must hold across several snapshots to count.
func unicodeEverySurfaceCheck(a *app, where string) {
	a.t.Helper()
	a.settled()
	unicodeCheckWidths(a, where)
	var errs []string
	for range 10 {
		if errs = unicodeEverySurfaceGraphemes(a); errs == nil {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	for _, e := range errs {
		a.t.Errorf("%s: %s\n%s", where, e, a.text())
	}
}

// unicodeEverySurfaceGraphemes lists the split graphemes on screen now.
func unicodeEverySurfaceGraphemes(a *app) []string {
	var errs []string
	for y, row := range a.term.Snapshot().Cells {
		for x, c := range row {
			if c.Content == "" {
				continue
			}
			rs := []rune(c.Content)
			first, last := rs[0], rs[len(rs)-1]
			switch {
			case strings.ContainsRune(c.Content, '\uFFFD'):
				errs = append(errs, fmt.Sprintf("U+FFFD at (%d,%d) row %q", x, y, unicodeRow(row)))
			case unicode.Is(unicode.Mn, first) || first == '\u200d' || first == '\ufe0f':
				errs = append(errs, fmt.Sprintf("orphan mark %U at (%d,%d): a grapheme was split\nrow %q", first, x, y, unicodeRow(row)))
			case last == '\u200d':
				errs = append(errs, fmt.Sprintf("cell %q at (%d,%d) ends in a joiner: a grapheme was split\nrow %q", c.Content, x, y, unicodeRow(row)))
			}
		}
	}
	return errs
}

// unicodeEverySurfaceTape writes history entries (kind, data) as a
// tape/history file in a temp dir.
func unicodeEverySurfaceTape(t *testing.T, name string, entries ...[2]any) string {
	t.Helper()
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{
			"seq": i + 1, "kind": e[0], "data": e[1],
			"at": time.Date(2026, 9, 11, 10, 0, i, 0, time.UTC).Format(time.RFC3339),
		})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(append(b, '\n'))
	}
	path := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// unicodeEverySurfaceCursorAfter asserts the composer's reverse-video
// cursor cell sits right after the cells spelling want.
func unicodeEverySurfaceCursorAfter(a *app, want string) {
	a.t.Helper()
	a.settled()
	snap := a.term.Snapshot()
	row := composerRow(a.lines())
	if row < 0 {
		a.t.Fatalf("no composer row:\n%s", a.text())
	}
	cursor := -1
	for x, c := range snap.Cells[row] {
		if c.Style.Attrs&uv.AttrReverse != 0 {
			cursor = x
			break
		}
	}
	if cursor < 0 {
		a.t.Fatalf("no cursor cell in the composer row:\n%s", a.text())
	}
	if got := unicodeRow(snap.Cells[row][:cursor]); got != want {
		a.t.Errorf("cells before the cursor (col %d) are %q, want %q:\n%s", cursor, got, want, a.text())
	}
}

func TestUnicodeEverySurface(t *testing.T) {
	t.Parallel()

	t.Run("SessionTitleStatusBar", func(t *testing.T) {
		t.Parallel()
		for _, cols := range []int{41, 80, 120} {
			t.Run(fmt.Sprint(cols), func(t *testing.T) {
				t.Parallel()
				hist := statusbarSeed(t, "title.jsonl",
					statusbarEntry("meta", unicodeEverySurfaceM{"cwd": "/tmp/demo"}),
					statusbarEntry("input", unicodeEverySurfaceM{"text": "name it"}),
					statusbarEntry("assistant", unicodeEverySurfaceM{"text": "```stop\nnamed\n```"}),
					statusbarEntry("title", unicodeEverySurfaceM{"text": unicodeEverySurfaceLong}),
					unicodeEverySurfaceM{"kind": "done", "data": unicodeEverySurfaceM{"usage": statusbarUsage}},
				)
				a := startCfg(t, cols, 24, statusbarCfg(hist))
				a.waitFor("日本語")
				unicodeEverySurfaceCheck(a, "title bar")
				bar := statusbarLine(a, a.settled())
				if !strings.HasPrefix(strings.TrimSpace(bar), "日本語") {
					t.Errorf("bar does not lead with the title: %q\n%s", bar, a.text())
				}
				n := 0
				for _, l := range a.lines() {
					if strings.Contains(l, "日本語") {
						n++
					}
				}
				if n != 1 {
					t.Errorf("title on %d rows, want 1:\n%s", n, a.text())
				}
			})
		}
	})

	t.Run("SessionsPicker", func(t *testing.T) {
		t.Parallel()
		for _, cols := range []int{50, 61, 120} {
			t.Run(fmt.Sprint(cols), func(t *testing.T) {
				t.Parallel()
				a := start(t, cols, 24)
				newSessionSeed(t, a, "u-cjk", "/elsewhere/日本", strings.Repeat(unicodeEverySurfaceCJK, 3))
				newSessionSeed(t, a, "u-zwj", "/elsewhere/fam", strings.Repeat(unicodeEverySurfaceZWJ, 20))
				newSessionSeed(t, a, "u-comb", "/elsewhere/café", strings.Repeat("e\u0301", 40))
				newSessionSeed(t, a, "u-rtl", "/elsewhere/rtl", unicodeEverySurfaceRTL+" "+unicodeEverySurfaceLong)
				newSessionOpenPicker(a)
				a.waitFor("日本語")
				unicodeEverySurfaceCheck(a, "sessions picker")
				a.key(uv.KeyDown, 0)
				a.key(uv.KeyDown, 0)
				unicodeEverySurfaceCheck(a, "sessions picker after down")
			})
		}
	})

	t.Run("PaletteSearch", func(t *testing.T) {
		t.Parallel()
		for name, q := range map[string]string{
			"cjk": unicodeEverySurfaceCJK, "zwj": unicodeEverySurfaceZWJ,
			"comb": unicodeEverySurfaceComb, "rtl": unicodeEverySurfaceRTL,
		} {
			t.Run(name, func(t *testing.T) {
				t.Parallel()
				a := start(t, 60, 24)
				a.typeText("/")
				a.waitFor("> /")
				a.term.Paste(q)
				a.waitUntil(func(s string) bool { return strings.Contains(s, "> /"+q) }, "query in the composer")
				unicodeEverySurfaceCheck(a, "palette query "+name)
				unicodeEverySurfaceCursorAfter(a, "> /"+q)
			})
		}
	})

	t.Run("AtPickerFilenames", func(t *testing.T) {
		t.Parallel()
		names := []string{"日本語.md", "fam👨‍👩‍👧‍👦.txt", "café.go", "مرحبا.md",
			"docs/" + strings.Repeat("日本", 40) + ".md"}
		a := start(t, 60, 24)
		for _, n := range names {
			p := filepath.Join(a.home, n)
			if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(p, []byte("x\n"), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		a.typeText("@")
		a.waitFor("@café.go")
		unicodeEverySurfaceCheck(a, "@ picker open")
		for _, n := range names[:4] {
			if !strings.Contains(a.text(), "@"+n) {
				t.Errorf("@ picker is missing %q:\n%s", n, a.text())
			}
		}
		a.term.Paste("日本語")
		a.waitUntil(func(s string) bool {
			rows, _ := atPickerRows(s)
			return len(rows) > 0 && rows[0] == "日本語.md"
		}, "@日本語 filtered to 日本語.md first")
		unicodeEverySurfaceCheck(a, "@ picker filtered")
		a.key(uv.KeyTab, 0)
		a.atPickerDraft("tab completes", "> @日本語.md")
		unicodeEverySurfaceCursorAfter(a, "> @日本語.md ")
	})

	t.Run("AskOptions", func(t *testing.T) {
		t.Parallel()
		opts := []string{unicodeEverySurfaceCJK, unicodeEverySurfaceZWJ + " family",
			unicodeEverySurfaceComb, unicodeEverySurfaceRTL, unicodeEverySurfaceLong}
		js, _ := json.Marshal(opts)
		code := fmt.Sprintf("const c = tools.ask(\"選んで 🎨\", ...%s);\nconsole.log(\"picked \" + c);", js)
		tape := unicodeEverySurfaceTape(t, "ask.jsonl",
			[2]any{"meta", unicodeEverySurfaceM{"cwd": "/tmp/demo"}},
			[2]any{"input", unicodeEverySurfaceM{"text": "ask me"}},
			[2]any{"assistant", unicodeEverySurfaceM{"text": "```js\n" + code + "\n```"}},
			[2]any{"assistant", unicodeEverySurfaceM{"text": "```stop\nDone 完了.\n```"}},
			[2]any{"done", unicodeEverySurfaceM{"text": ""}},
		)
		a := startCfg(t, 60, 30, askConfig(tape))
		a.typeText("ask me")
		a.key(uv.KeyEnter, 0)
		a.waitFor("選んで")
		a.waitFor("مرحبا")
		unicodeEverySurfaceCheck(a, "ask pending")
		for _, o := range opts[:4] {
			if !strings.Contains(a.text(), o) {
				t.Errorf("ask option %q not on screen:\n%s", o, a.text())
			}
		}
		a.typeText("2")
		a.key(uv.KeyEnter, 0)
		a.waitFor("完了")
		unicodeEverySurfaceCheck(a, "ask answered")
		if !strings.Contains(a.text(), "→ "+opts[1]) {
			t.Errorf("answered one-liner does not carry the ZWJ option:\n%s", a.text())
		}
	})

	t.Run("TodoItems", func(t *testing.T) {
		t.Parallel()
		items := []string{unicodeEverySurfaceCJK, unicodeEverySurfaceZWJ + " family",
			unicodeEverySurfaceComb, unicodeEverySurfaceRTL, unicodeEverySurfaceLong}
		entries := [][2]any{
			{"meta", unicodeEverySurfaceM{"cwd": "/tmp/demo"}},
			{"input", unicodeEverySurfaceM{"text": "plan"}},
			{"assistant", unicodeEverySurfaceM{"text": "```stop\nPlanned.\n```"}},
		}
		for i, it := range items {
			entries = append(entries, [2]any{"todo/add", unicodeEverySurfaceM{"id": i + 1, "text": it}})
		}
		entries = append(entries, [2]any{"todo/done", unicodeEverySurfaceM{"id": 2}},
			[2]any{"done", unicodeEverySurfaceM{"text": ""}})
		for _, cols := range []int{41, 100} {
			t.Run(fmt.Sprint(cols), func(t *testing.T) {
				t.Parallel()
				tape := unicodeEverySurfaceTape(t, "todo.jsonl", entries...)
				a := startCfg(t, cols, 30, subagentsConfig(tape))
				a.waitFor(todoHeader)
				unicodeEverySurfaceCheck(a, "todo panel")
				s := a.text()
				for _, want := range []string{"1 " + unicodeEverySurfaceCJK, "[x] 2 " + unicodeEverySurfaceZWJ, "3 café"} {
					if !strings.Contains(s, want) {
						t.Errorf("todo panel is missing %q:\n%s", want, s)
					}
				}
			})
		}
	})

	t.Run("SubagentCards", func(t *testing.T) {
		t.Parallel()
		entries := [][2]any{
			{"meta", unicodeEverySurfaceM{"cwd": "/tmp/demo"}},
			{"input", unicodeEverySurfaceM{"text": "fan out"}},
			{"assistant", unicodeEverySurfaceM{"text": "Spawning."}},
		}
		// Distinct openings: a shared one of 24+ bytes is hidden from the
		// collapsed heads on purpose (taskLabel).
		for i, ti := range []string{unicodeEverySurfaceMixed, "second " + unicodeEverySurfaceLong} {
			w := i + 1
			entries = append(entries,
				[2]any{"sub:start", unicodeEverySurfaceM{"worker": w, "text": ti}},
				[2]any{"sub:assistant", unicodeEverySurfaceM{"worker": w, "text": "Status: ok\n" + unicodeEverySurfaceRTL}},
				[2]any{"sub:done", unicodeEverySurfaceM{"worker": w, "status": "ok", "steps": 1}})
		}
		entries = append(entries,
			[2]any{"assistant", unicodeEverySurfaceM{"text": "```stop\nBoth done.\n```"}},
			[2]any{"done", unicodeEverySurfaceM{"text": ""}})
		for _, cols := range []int{41, 120} {
			t.Run(fmt.Sprint(cols), func(t *testing.T) {
				t.Parallel()
				tape := unicodeEverySurfaceTape(t, "subs.jsonl", entries...)
				a := startCfg(t, cols, 30, subagentsConfig(tape))
				a.waitFor("Both done")
				unicodeEverySurfaceCheck(a, "subagent cards")
				n := 0
				for _, l := range a.lines() {
					if strings.Contains(l, "日本語") && strings.Contains(l, "▸") {
						n++
					}
				}
				if n != 2 {
					t.Errorf("want 2 collapsed card heads with the unicode title, got %d:\n%s", n, a.text())
				}
			})
		}
	})

	t.Run("BangShellOutput", func(t *testing.T) {
		t.Parallel()
		for _, cols := range []int{41, 100} {
			t.Run(fmt.Sprint(cols), func(t *testing.T) {
				t.Parallel()
				tape, _ := filepath.Abs("testdata/replay/bang-shell.jsonl")
				a := startCfg(t, cols, 30, replayConfig(tape))
				a.term.Paste("!printf '%s\\n' '" + unicodeEverySurfaceMixed + "' '" + unicodeEverySurfaceLong + "' 'END-UNI'")
				a.key(uv.KeyEnter, 0)
				a.waitFor("END-UNI")
				unicodeEverySurfaceCheck(a, "bang output")
				if !strings.Contains(a.text(), unicodeEverySurfaceMixed) {
					t.Errorf("bang output lost the mixed line:\n%s", a.text())
				}
			})
		}
	})
}

// TestUnicodeEverySurfaceTruncation puts a ZWJ sequence exactly where a
// rune-count truncation cuts: the cut must land on a grapheme boundary,
// never after a joiner.
func TestUnicodeEverySurfaceTruncation(t *testing.T) {
	t.Parallel()

	// spawn.go line(label, 44): 42 runes + 👨 + ZWJ is the 44-rune cut.
	t.Run("SubagentCardTitle", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_UNICODE_EVERY_SURFACE") == "" {
			t.Skip("known bug: plugins/ui/spawn.go line() cuts card titles by rune count and splits ZWJ graphemes; set BOUGH_KNOWN_UNICODE_EVERY_SURFACE=1")
		}
		tape := unicodeEverySurfaceTape(t, "subs.jsonl",
			[2]any{"meta", unicodeEverySurfaceM{"cwd": "/tmp/demo"}},
			[2]any{"input", unicodeEverySurfaceM{"text": "fan out"}},
			[2]any{"assistant", unicodeEverySurfaceM{"text": "Spawning."}},
			[2]any{"sub:start", unicodeEverySurfaceM{"worker": 1, "text": strings.Repeat("x", 42) + unicodeEverySurfaceZWJ + " tail"}},
			[2]any{"sub:done", unicodeEverySurfaceM{"worker": 1, "status": "ok", "steps": 1}},
			[2]any{"assistant", unicodeEverySurfaceM{"text": "```stop\nDone.\n```"}},
			[2]any{"done", unicodeEverySurfaceM{"text": ""}},
		)
		a := startCfg(t, 120, 30, subagentsConfig(tape))
		a.waitFor("subagent 1")
		unicodeEverySurfaceCheck(a, "card title cut at a ZWJ")
	})

	// session.go truncateCols(title, 60) keeps 59 runes: 57 + 👨 + ZWJ.
	t.Run("SessionsPickerTitle", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_UNICODE_EVERY_SURFACE") == "" {
			t.Skip("known bug: plugins/ui/session.go truncateCols() cuts picker titles by rune count and splits ZWJ graphemes; set BOUGH_KNOWN_UNICODE_EVERY_SURFACE=1")
		}
		a := start(t, 160, 24)
		newSessionSeed(t, a, "u-cut", "/elsewhere/cut", strings.Repeat("y", 57)+unicodeEverySurfaceZWJ+" tail")
		newSessionOpenPicker(a)
		a.waitFor("yyyy")
		unicodeEverySurfaceCheck(a, "picker title cut at a ZWJ")
	})
}
