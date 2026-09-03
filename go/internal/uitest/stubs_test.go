package uitest_test

// Provider behaviours and tool-output shapes through the real loop,
// codemode and tools-basic, rendered by the real ui model.

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/tools"
)

const (
	cols = 100
	rows = 40
)

func mountLLM(t *testing.T, stub any, extra ...string) *uitest.Driver {
	t.Helper()
	return uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		append([]string{"codemode", "tools-basic", "loop"}, extra...)...)
}

// fits asserts the frame invariants every state must satisfy: no
// render panic, every line within the width, no control characters.
func fits(t *testing.T, d *uitest.Driver) {
	t.Helper()
	raw := d.RawFrame()
	if strings.Contains(raw, "✗ render failed") {
		t.Fatalf("render panicked:\n%s", d.Frame())
	}
	for i, ln := range strings.Split(raw, "\n") {
		if w := ansi.StringWidth(ln); w > cols {
			t.Fatalf("line %d is %d cells wide (max %d):\n%s", i, w, cols, ansi.Strip(ln))
		}
		for _, r := range ansi.Strip(ln) {
			if (r < 0x20 && r != 0x1b) || r == 0x7f {
				t.Fatalf("line %d carries control char %q:\n%q", i, r, ansi.Strip(ln))
			}
		}
	}
}

// turnDone pumps until the turn's done marker lands: the composer is
// back to idle (no spinner, no queued prompt) and the frame is stable.
func turnDone(d *uitest.Driver, marker string) {
	d.WaitFor(marker)
	d.WaitUntil(func(f string) bool { return !spinnerIn(f) }, "spinner to stop")
}

func spinnerIn(s string) bool {
	for _, f := range []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"} {
		if strings.Contains(s, f) {
			return true
		}
	}
	return false
}

// --- providers ---

// Streaming with every chunking: one code block, rendered once, and
// the surrounding prose intact — chunk boundaries inside a fence or a
// rune never leak a partial fence or mojibake into the transcript.
func TestStreamingChunkingsRenderOneCodeBlock(t *testing.T) {
	t.Parallel()
	reply := "Résumé → 日本語 first.\n```js\ntools.bash(\"echo chunked\")\n```\nAnd after 🐛."
	for name, ch := range map[string]uitest.Chunker{
		"whole": uitest.Whole, "rune": uitest.ByRune, "by3": uitest.ByN(3), "by7": uitest.ByN(7),
		"bytes2": uitest.ByBytes(2), "bytes5": uitest.ByBytes(5),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			stub := &uitest.Streaming{Script: uitest.Script{Replies: []string{reply, "final."}}, Chunk: ch}
			d := mountLLM(t, stub)
			d.Say("go")
			turnDone(d, "final.")
			f := d.Frame()
			fits(t, d)
			for _, want := range []string{"Résumé → 日本語 first.", "▸ Ran: echo chunked", "chunked", "And after 🐛.", "final."} {
				if !strings.Contains(f, want) {
					t.Fatalf("%s: frame missing %q:\n%s", name, want, f)
				}
			}
			if n := strings.Count(f, "▸ Ran"); n != 1 {
				t.Fatalf("%s: %d code blocks, want 1:\n%s", name, n, f)
			}
			if strings.Contains(f, "�") {
				t.Fatalf("%s: replacement char in frame (a delta split a rune):\n%s", name, f)
			}
			if strings.Contains(f, "```") {
				t.Fatalf("%s: raw fence in frame:\n%s", name, f)
			}
		})
	}
}

// A streamed delta that is not valid UTF-8 on its own (byte chunking)
// must not show up mid-turn as a replacement character either.
func TestStreamingByteChunksNeverShowMojibakeLive(t *testing.T) {
	t.Parallel()
	// Short enough that one-byte deltas stay under the harness's event
	// buffer (256): a flood of deltas would drop the done event.
	reply := strings.Repeat("日本語🐛 ", 8)
	gate := make(chan struct{})
	stub := &gated{Streaming: uitest.Streaming{Script: uitest.Script{Replies: []string{reply, "end"}}, Chunk: uitest.ByBytes(1)}, gate: gate}
	d := mountLLM(t, stub)
	d.Say("go")
	d.WaitFor("日本語")
	for i := 0; i < 20; i++ {
		d.Step()
		if strings.Contains(d.Frame(), "�") {
			t.Fatalf("mojibake in the live block:\n%s", d.Frame())
		}
		time.Sleep(5 * time.Millisecond)
	}
	close(gate)
	turnDone(d, "🐛")
	fits(t, d)
}

// gated holds the stream open after a dozen deltas so the live block
// can be observed mid-stream.
type gated struct {
	uitest.Streaming
	gate chan struct{}
}

func (g *gated) Stream(ctx context.Context, sys string, msgs []llm.Message, onDelta func(string)) (string, error) {
	n := 0
	return g.Streaming.Stream(ctx, sys, msgs, func(d string) {
		onDelta(d)
		if n++; n == 9 {
			select {
			case <-g.gate:
			case <-ctx.Done():
			}
		}
	})
}

// The provider fails: the turn shows the error, ends with a done
// marker, and the composer takes the next prompt.
func TestProviderErrorEndsTheTurn(t *testing.T) {
	t.Parallel()
	stub := &uitest.Failing{Err: uitest.ErrProvider}
	d := mountLLM(t, stub)
	d.Say("hello")
	turnDone(d, "503 overloaded")
	fits(t, d)
	f := d.Frame()
	if !strings.Contains(f, "✗") {
		t.Fatalf("no error marker:\n%s", f)
	}
	// Next prompt still works: the failure did not wedge the loop.
	d.Say("again")
	turnDone(d, "503 overloaded") // still failing, but a second turn ran
	if n := strings.Count(d.Frame(), "503 overloaded"); n != 2 {
		t.Fatalf("second turn did not run (%d errors):\n%s", n, d.Frame())
	}
}

// Fails once, then answers: the transient error is visible and the
// retry (the user's next prompt) succeeds.
func TestProviderRecoversAfterFailure(t *testing.T) {
	t.Parallel()
	stub := &uitest.Failing{Script: uitest.Script{Replies: []string{"recovered"}}, Err: errors.New("provider: connection reset"), After: 1}
	d := mountLLM(t, stub)
	d.Say("one")
	turnDone(d, "connection reset")
	d.Say("two")
	turnDone(d, "recovered")
	fits(t, d)
	f := d.Frame()
	if strings.Index(f, "connection reset") > strings.Index(f, "recovered") {
		t.Fatalf("turns out of order:\n%s", f)
	}
}

// A hung provider is cancelled with esc: the cancelled marker lands,
// the spinner stops, and the composer takes the next prompt.
func TestSlowProviderCancelledByEsc(t *testing.T) {
	t.Parallel()
	slow := uitest.NewSlow()
	d := mountLLM(t, slow)
	d.Say("wait")
	<-slow.Started
	d.Press("esc")
	turnDone(d, "cancelled")
	fits(t, d)
	if strings.Contains(d.Frame(), "late") {
		t.Fatalf("cancelled reply still rendered:\n%s", d.Frame())
	}
}

// ctrl+c cancels the same way, and the second ctrl+c after the turn
// ended arms quit rather than quitting.
func TestSlowProviderCancelledByCtrlC(t *testing.T) {
	t.Parallel()
	slow := uitest.NewSlow()
	d := mountLLM(t, slow)
	d.Say("wait")
	<-slow.Started
	d.Press("ctrl+c")
	turnDone(d, "cancelled")
	d.Press("ctrl+c")
	d.Step()
	if !strings.Contains(d.Frame(), "ctrl+c") { // the "press again" hint
		t.Fatalf("ctrl+c after a cancel should only arm quit:\n%s", d.Frame())
	}
}

// A prompt typed while a turn runs is queued and runs after it.
func TestPromptQueuedDuringTurn(t *testing.T) {
	t.Parallel()
	slow := uitest.NewSlow()
	d := mountLLM(t, slow)
	d.Say("first")
	<-slow.Started
	d.Say("second")
	d.Step()
	if !strings.Contains(d.Frame(), "second") {
		t.Fatalf("queued prompt not echoed:\n%s", d.Frame())
	}
	close(slow.Release)
	turnDone(d, "late")
	fits(t, d)
}

// A three-step tool chain renders code/result pairs in order with the
// final prose last.
func TestThreeStepToolChain(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{
		uitest.Bash("echo step-one"), uitest.Bash("echo step-two"), uitest.Bash("echo step-three"), "all three ran",
	}}
	d := mountLLM(t, stub)
	d.Say("chain")
	turnDone(d, "all three ran")
	fits(t, d)
	f := d.Frame()
	last := -1
	for _, want := range []string{"echo step-one", "echo step-two", "echo step-three", "all three ran"} {
		i := strings.Index(f, want)
		if i < 0 || i < last {
			t.Fatalf("%q missing or out of order:\n%s", want, f)
		}
		last = i
	}
	if stub.Calls != 4 {
		t.Fatalf("llm called %d times, want 4", stub.Calls)
	}
}

// Two fences in one reply run in order; a reply with a fence the loop
// must not run (indented, quoted) is prose only.
func TestFenceVariants(t *testing.T) {
	t.Parallel()
	cases := map[string]struct {
		reply string
		ran   int
		end   string // what marks the turn's end on screen
	}{
		"two":       {uitest.Bash("echo A") + "\nthen\n" + uitest.Bash("echo B"), 2, "end-two"},
		"non-js":    {"```python\nprint('x')\n```\nnot run", 0, "not run"},
		"unclosed":  {"```js\ntools.bash(\"echo U\")", 0, "echo U"},
		"tilde":     {"~~~js\ntools.bash(\"echo T\")\n~~~\nnot run", 0, "not run"},
		"uppercase": {"```JS\ntools.bash(\"echo C\")\n```\nnot run", 0, "not run"},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			stub := &uitest.Script{Replies: []string{c.reply, "end-" + name}}
			d := mountLLM(t, stub)
			d.Say("x")
			turnDone(d, c.end)
			fits(t, d)
			if n := strings.Count(d.Frame(), "▸ Ran"); n != c.ran {
				t.Fatalf("%s: %d code blocks ran, want %d:\n%s", name, n, c.ran, d.Frame())
			}
		})
	}
}

// --- tool outputs ---

// Every output shape a shell can produce renders inside the frame with
// no control characters, and the turn ends.
func TestToolOutputShapes(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name, cmd string
		want      []string // in the frame after expanding the result
		absent    []string
	}{
		{"empty", "true", nil, []string{"✗"}},
		{"stderr", "echo to-stderr 1>&2", []string{"to-stderr"}, nil},
		{"exit-code", "echo partial; exit 3", []string{"exit status 3", "partial"}, nil},
		{"many-lines", "seq 1 3000", []string{"[truncated]"}, nil}, // 8 KiB cap; the cut is announced at the box's end
		{"long-line", "head -c 12000 /dev/zero | tr '\\0' x", []string{"xxxxxxxx"}, nil},
		{"ansi", "printf '\\033[1;31mred\\033[0m plain'", []string{"red plain"}, []string{"[1;31m", "[0m"}},
		{"osc-link", "printf '\\033]8;;http://x\\033\\\\linked\\033]8;;\\033\\\\'", []string{"linked"}, []string{"]8;;", "http://x"}},
		{"cr-progress", "printf '10%%\\r50%%\\r100%%\\n'", []string{"100%"}, []string{"10%", "50%"}},
		{"tabs", "printf 'a\\tb\\tc'", []string{"a    b    c"}, nil},
		{"unicode", "printf '日本語 🐛 e\\xcc\\x81 →'", []string{"日本語 🐛", "→"}, nil},
		{"nul", "printf 'hello\\0world and more text after it'", []string{"helloworld and more"}, nil},
		{"binary", "head -c 200 /dev/urandom", []string{"(binary, 200 bytes)"}, nil},
		{"json", `printf '{"a":[1,2,{"b":"c"}]}'`, []string{`{"a":[1,2,{"b":"c"}]}`}, nil},
		{"markdown-ish", "printf '# not a heading\\n- not a list\\n| not | table |'", []string{"# not a heading", "- not a list", "| not | table |"}, nil},
		{"wide-cols", "printf '%s' " + strings.Repeat("日", 150), []string{"日日日日"}, nil},
		{"only-newlines", "printf '\\n\\n\\n'", nil, []string{"✗"}},
		{"trailing-spaces", "printf 'x   \\ny  '", []string{"x", "y"}, nil},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			stub := &uitest.Script{Replies: []string{uitest.Bash(c.cmd), "done-" + c.name}}
			d := mountLLM(t, stub)
			d.Say("run")
			turnDone(d, "done-"+c.name)
			fits(t, d)
			// Expand every collapsed block, then jump to the transcript's
			// end so a long box's tail (the truncation note) is on screen.
			for n := strings.Count(d.Frame(), "▸ "); n > 0; n-- {
				d.Press("tab", "enter") // tab walks one block per press; enter toggles it
				fits(t, d)
			}
			d.Press("end")
			f := d.Frame()
			for _, w := range c.want {
				if !strings.Contains(f, w) {
					t.Fatalf("%s: frame missing %q:\n%s", c.name, w, f)
				}
			}
			// The code header and box echo the command source (escape
			// text and all); the absent strings are about the RESULT
			// section: from its header to the next block.
			var results []string
			in := false
			for _, ln := range strings.Split(f, "\n") {
				switch {
				case strings.Contains(ln, "▾ result") || strings.Contains(ln, "▸ result"):
					in = true
				case strings.HasPrefix(ln, "●") || strings.HasPrefix(ln, "▸") || strings.HasPrefix(ln, "▾") || strings.HasPrefix(ln, "❯"):
					in = false
				}
				if in {
					results = append(results, ln)
				}
			}
			rf := strings.Join(results, "\n")
			for _, a := range c.absent {
				if strings.Contains(rf, a) {
					t.Fatalf("%s: result rows must not contain %q:\n%s", c.name, a, f)
				}
			}
		})
	}
}

// The exit chip on the done marker reflects the last bash exit.
func TestDoneChipShowsExit(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{uitest.Bash("exit 7"), "after"}}
	d := mountLLM(t, stub)
	d.Say("x")
	turnDone(d, "after")
	if f := d.Frame(); !strings.Contains(f, "✗ exit 7") {
		t.Fatalf("done chip missing exit 7:\n%s", f)
	}
}

// A file written by tools.patch shows on the done marker.
func TestDoneChipShowsWrittenFile(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := dir + "/note.txt"
	stub := &uitest.Script{Replies: []string{uitest.JS(fmt.Sprintf("tools.patch(%q, \"\", \"hello\")", path)), "wrote it"}}
	d := mountLLM(t, stub)
	d.Say("x")
	turnDone(d, "wrote it")
	fits(t, d)
	if f := d.Frame(); !strings.Contains(f, "✔ wrote") {
		t.Fatalf("done chip missing the written file:\n%s", f)
	}
}

// A JS error in the block (not a tool error) renders as an error block
// and the turn still ends.
func TestCodeErrorRendersAndEnds(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{uitest.JS("throw new Error('boom from js')"), "recovered after js error"}}
	d := mountLLM(t, stub)
	d.Say("x")
	turnDone(d, "recovered after js error")
	fits(t, d)
	if f := d.Frame(); !strings.Contains(f, "boom from js") {
		t.Fatalf("js error not shown:\n%s", f)
	}
}

// A bash command that ignores SIGTERM is killed on cancel, the block
// shows it was cancelled, and the loop is free for the next prompt.
func TestBashKilledOnCancel(t *testing.T) {
	t.Parallel()
	stub := &uitest.Script{Replies: []string{uitest.Bash("sleep 30"), "never"}}
	d := mountLLM(t, stub)
	d.Say("x")
	d.WaitFor("sleep 30")
	d.Press("esc")
	turnDone(d, "cancelled")
	fits(t, d)
	if strings.Contains(d.Frame(), "never") {
		t.Fatalf("turn continued after cancel:\n%s", d.Frame())
	}
}
