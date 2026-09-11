package vtreal

// Rapid over tapes. Two properties, both on the real PTY through the
// replay plugin:
//
//   - TestFuzzTapesInputs draws a random subset and ordering of a
//     tape's inputs, a random pane size, and random wheel/click
//     interleavings between turns.
//   - TestFuzzTapesGeneratedProperty generates a whole tape (random
//     entry kinds, widths, unicode, fences), writes it as jsonl to a
//     temp dir and replays it.
//
// Both boot the binary per check, so both are opt-in:
//
//	BOUGH_FUZZ_TAPES=1 go test ./internal/vtreal -run TestFuzzTapes -rapid.checks=20
//
// TestFuzzTapesGenerated is the fixture-scale version of the second:
// two seeded tapes, always run. Every failure prints the tape path and
// the screen.

import (
	"encoding/json"
	"fmt"
	"math/rand"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"pgregory.net/rapid"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/replay"
)

// --- tape generator ---

// fuzzTapesText returns a random chunk of prose: plain words, wide
// CJK, combining marks and emoji, plus the occasional unbroken run
// that no wrapper can break at a space.
func fuzzTapesText(r *rand.Rand) string {
	atoms := []string{
		"hello", "ok", "файл", "日本語のテキスト", "🙂🙃👩‍👩‍👧‍👦", "café", "é", "a\tb",
		"~!@#$%^&*()_+", "«quoted»", "тест", "한국어", "0123456789",
		strings.Repeat("x", 1+r.Intn(200)),
		strings.Repeat("日", 1+r.Intn(60)),
		"https://example.com/" + strings.Repeat("segment/", 1+r.Intn(20)),
	}
	var sb strings.Builder
	for n := 1 + r.Intn(6); n > 0; n-- {
		sb.WriteString(atoms[r.Intn(len(atoms))])
		if r.Intn(4) == 0 {
			sb.WriteString("\n")
		} else {
			sb.WriteString(" ")
		}
	}
	return strings.TrimSpace(sb.String())
}

// fuzzTapesGenerate writes a random tape to dir and returns its path.
// The shape is a real session's: meta, then turns of input / assistant
// / code / result, ending each turn with a stop fence and a done, with
// noise entries (thinking, system, todo, sub:*) sprinkled in — the
// replay plugin must ignore those and the UI must survive them.
func fuzzTapesGenerate(t testing.TB, dir string, seed int64, turns int) string {
	t.Helper()
	r := rand.New(rand.NewSource(seed))
	var entries []history.Entry
	seq := int64(0)
	at := time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC)
	add := func(kind string, data map[string]any) {
		seq++
		at = at.Add(time.Second)
		entries = append(entries, history.Entry{Seq: seq, At: at, Kind: kind, Data: data})
	}
	noise := []string{"thinking", "system", "todo/add", "todo/done", "job", "title", "nudge", "sub:start", "sub:assistant", "sub:done"}

	add("meta", map[string]any{"cwd": dir})
	for i := range turns {
		add("input", map[string]any{"text": fuzzTapesText(r)})
		if r.Intn(3) == 0 {
			add(noise[r.Intn(len(noise))], map[string]any{"text": fuzzTapesText(r)})
		}
		for blocks := r.Intn(3); blocks > 0; blocks-- {
			code := fmt.Sprintf("console.log(%q) // %d\n", fuzzTapesText(r), i)
			add("assistant", map[string]any{"text": "```js\n" + code + "```"})
			add("code", map[string]any{"text": code})
			out := fuzzTapesText(r)
			if r.Intn(4) == 0 {
				out = "error: " + out
			}
			add("result", map[string]any{"code": code, "text": out})
		}
		add("assistant", map[string]any{"text": fuzzTapesText(r) + "\n\n```stop\n" + fuzzTapesText(r) + "\n```"})
		add("done", map[string]any{"text": ""})
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
	path := filepath.Join(dir, fmt.Sprintf("gen-%d.jsonl", seed))
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// --- shared driving ---

// fuzzTapesCheck is app.check, reported through rapid so a failure
// shrinks, and with the tape path in the message.
func fuzzTapesCheck(rt *rapid.T, a *app, tape, where string) {
	s := a.settled()
	ls := strings.Split(s, "\n")
	fail := func(why string) {
		rt.Fatalf("%s: %s\ntape: %s\nscreen:\n%s", where, why, tape, s)
	}
	if panicky.MatchString(s) {
		fail("crash text on screen")
	}
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		fail("composer not on the last rows")
	}
	if !strings.Contains(s, "? keys") {
		fail("status bar missing")
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > a.cols {
			fail(fmt.Sprintf("row %d is %d cells wide in %d columns", i, w, a.cols))
		}
	}
}

// fuzzTapesSend types one input, waits for the turn to land and checks.
func fuzzTapesSend(rt *rapid.T, a *app, tape, in string, turn int) {
	if strings.Contains(in, "\n") || len(in) > 200 {
		a.term.Paste(in)
	} else {
		a.typeText(in)
	}
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(turn, 60*time.Second) {
		rt.Fatalf("turn %d never finished\ntape: %s\nscreen:\n%s", turn, tape, a.text())
	}
	fuzzTapesCheck(rt, a, tape, fmt.Sprintf("after turn %d", turn))
}

// fuzzTapesInterleave sends a random burst of wheel and click events
// between turns, then checks.
func fuzzTapesInterleave(rt *rapid.T, a *app, tape string, n int) {
	for range n {
		if rapid.Bool().Draw(rt, "wheel") {
			b := uv.MouseWheelUp
			if rapid.Bool().Draw(rt, "down") {
				b = uv.MouseWheelDown
			}
			for range rapid.IntRange(1, 6).Draw(rt, "ticks") {
				a.term.SendMouse(uv.MouseWheelEvent{X: 3, Y: 2, Button: b})
			}
			continue
		}
		// Transcript rows only: the status bar and composer open
		// pickers, which are their own flows.
		a.click(rapid.IntRange(0, a.cols-1).Draw(rt, "x"), rapid.IntRange(0, max(a.rows-3, 1)).Draw(rt, "y"))
		if s := a.settled(); strings.Contains(s, "esc back") || strings.Contains(s, "esc to close") {
			a.key(uv.KeyEscape, 0)
		}
	}
	fuzzTapesCheck(rt, a, tape, fmt.Sprintf("after %d mouse bursts", n))
}

func fuzzTapesSize(rt *rapid.T) (int, int) {
	return rapid.SampledFrom([]int{40, 80, 100, 160}).Draw(rt, "cols"),
		rapid.SampledFrom([]int{10, 24, 40}).Draw(rt, "rows")
}

func fuzzTapesGate(t *testing.T) {
	if os.Getenv("BOUGH_FUZZ_TAPES") == "" {
		t.Skip("set BOUGH_FUZZ_TAPES=1")
	}
}

// --- properties ---

// TestFuzzTapesInputs replays a fixture tape's inputs in a random
// subset and order, at a random size, with mouse noise between turns.
// The replies come off the tape in recorded order whatever the input
// order is, so this is a property about the UI, not about the answers.
func TestFuzzTapesInputs(t *testing.T) {
	fuzzTapesGate(t)
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	var inputs []string
	for _, in := range tp.Inputs {
		if !strings.HasPrefix(in, "/") { // a command: never went to the model
			inputs = append(inputs, in)
		}
	}
	if len(inputs) == 0 {
		t.Fatalf("tape %s has no model inputs", tape)
	}
	rapid.Check(t, func(rt *rapid.T) {
		cols, rows := fuzzTapesSize(rt)
		a := startCfg(t, cols, rows, replayConfig(tape))
		fuzzTapesCheck(rt, a, tape, "boot")
		n := rapid.IntRange(1, len(inputs)+1).Draw(rt, "turns")
		for turn := 1; turn <= n; turn++ {
			in := inputs[rapid.IntRange(0, len(inputs)-1).Draw(rt, "input")]
			fuzzTapesSend(rt, a, tape, in, turn)
			if k := rapid.IntRange(0, 2).Draw(rt, "mouse"); k > 0 {
				fuzzTapesInterleave(rt, a, tape, k)
			}
		}
	})
}

// TestFuzzTapesGeneratedProperty replays a freshly generated tape.
func TestFuzzTapesGeneratedProperty(t *testing.T) {
	fuzzTapesGate(t)
	t.Parallel()
	dir := t.TempDir()
	rapid.Check(t, func(rt *rapid.T) {
		cols, rows := fuzzTapesSize(rt)
		seed := rapid.Int64().Draw(rt, "seed")
		turns := rapid.IntRange(1, 4).Draw(rt, "turns")
		tape := fuzzTapesGenerate(t, dir, seed, turns)
		tp, err := replay.Load(tape)
		if err != nil {
			rt.Fatalf("load: %v\ntape: %s", err, tape)
		}
		a := startCfg(t, cols, rows, replayConfig(tape))
		fuzzTapesCheck(rt, a, tape, "boot")
		for i, in := range tp.Inputs {
			fuzzTapesSend(rt, a, tape, in, i+1)
			if k := rapid.IntRange(0, 1).Draw(rt, "mouse"); k > 0 {
				fuzzTapesInterleave(rt, a, tape, k)
			}
		}
	})
}

// TestFuzzTapesGenerated is the always-on, fixture-scale version:
// two seeded generated tapes through the same drive() the recorded
// fixtures use.
func TestFuzzTapesGenerated(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	for _, seed := range []int64{1, 2} {
		tape := fuzzTapesGenerate(t, dir, seed, 2)
		t.Run(fmt.Sprintf("seed%d", seed), func(t *testing.T) {
			t.Parallel()
			drive(t, tape, 100, 30)
		})
	}
}
