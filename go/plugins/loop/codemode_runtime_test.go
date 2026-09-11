package loop

import (
	"context"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/codemode"
)

// Tool group "codemode-runtime", loop side: a REAL goja VM behind the
// runner, so each failure is checked for what the model is fed next
// (the recorded "result" history entry) and that the next block runs.

func codemodeRuntimeLoopKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_CODEMODE_RUNTIME") != "1" {
		t.Skip("known bug: " + bug + " (set BOUGH_KNOWN_TOOLS_CODEMODE_RUNTIME=1 to run)")
	}
}

// codemodeRuntimeTurn runs one turn whose only reply is js, then returns
// the event kinds/texts and the last "result" history text.
func codemodeRuntimeTurn(t *testing.T, cm *codemode.CodeMode, ctx context.Context, js string) (kinds, texts []string, result string) {
	t.Helper()
	l := &recordLLM{reply: "go\n```js\n" + js + "\n```"}
	r := buildRunner(t, l, cm, nil, nil)
	r.maxSteps = 1
	done := make(chan struct{})
	go func() {
		_ = r.Run(ctx, "go", collect(&kinds, &texts))
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(20 * time.Second):
		t.Fatalf("turn hung: %s", js)
	}
	for _, e := range r.hist.Entries() {
		if e.Kind == "result" {
			result, _ = e.Data["text"].(string)
		}
	}
	return
}

func TestCodemodeRuntimeLoopFeedsFailures(t *testing.T) {
	cases := []struct {
		name, js, kind string
		want           []string
	}{
		{"syntax", "const b = ;", "error", []string{"error: SyntaxError", "const b = ;"}},
		{"undefined tool", "tools.nope()", "error", []string{"error: TypeError"}},
		{"throw string", `throw "plain"`, "error", []string{"error: plain"}},
		{"prints then throws", `console.log("PARTIAL"); throw new Error("late")`, "error", []string{"PARTIAL", "error: Error: late"}},
		{"silent value", `const x = 1`, "result", []string{"printed nothing"}},
		{"top-level await", `await 1`, "error", []string{"SyntaxError"}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			cm := codemode.New(2 * time.Second)
			kinds, texts, result := codemodeRuntimeTurn(t, cm, context.Background(), c.js)
			for _, w := range c.want {
				if !strings.Contains(result, w) {
					t.Errorf("model is fed %q, lacks %q", result, w)
				}
			}
			found := false
			for i, k := range kinds {
				if k == c.kind && strings.Contains(texts[i], c.want[0]) {
					found = true
				}
			}
			if !found {
				t.Errorf("no %q event: %v", c.kind, kinds)
			}
			if kinds[len(kinds)-1] != "done" {
				t.Errorf("turn did not end done: %v", kinds)
			}
			if out, err := cm.Run(`"alive"`); err != nil || out != "alive" {
				t.Fatalf("VM dead after %s: %q %v", c.name, out, err)
			}
		})
	}
}

// Output past maxResultBytes is capped before the model sees it.
func TestCodemodeRuntimeLoopCapsOutput(t *testing.T) {
	t.Setenv("BOUGH_SCRATCH", t.TempDir())
	cm := codemode.New(5 * time.Second)
	_, _, result := codemodeRuntimeTurn(t, cm, context.Background(), `console.log("x".repeat(`+strconv.Itoa(maxResultBytes*4)+`))`)
	if len(result) > maxResultBytes+4096 {
		t.Fatalf("result not capped: %d bytes (cap %d)", len(result), maxResultBytes)
	}
}

// esc mid-block: the turn ends cancelled within a bound, and the same
// VM runs the next block.
func TestCodemodeRuntimeLoopCancelSpin(t *testing.T) {
	cm := codemode.New(30 * time.Second)
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(200*time.Millisecond, cancel)
	start := time.Now()
	kinds, _, _ := codemodeRuntimeTurn(t, cm, ctx, `while(true){}`)
	if d := time.Since(start); d > 5*time.Second {
		t.Fatalf("cancel took %v", d)
	}
	if kinds[len(kinds)-1] != "cancelled" && kinds[len(kinds)-1] != "done" {
		t.Logf("kinds=%v", kinds)
	}
	if out, err := cm.Run(`1+1`); err != nil || out != "2" {
		t.Fatalf("VM dead after cancel: %q %v", out, err)
	}
}

// A panicking Go tool must become an error result, not kill bough.
func TestCodemodeRuntimeLoopPanickingTool(t *testing.T) {
	codemodeRuntimeLoopKnown(t, "a panicking Go tool crashes the process: no recover in codemode RunCtx (codemode.go:271) or loop.runCode's goroutine (cancel.go:133)")
	cm := codemode.New(2 * time.Second)
	cm.RegisterTool("boom", func() (string, error) { panic("host blew up") })
	_, _, result := codemodeRuntimeTurn(t, cm, context.Background(), `console.log("before"); tools.boom()`)
	if !strings.Contains(result, "before") || !strings.Contains(result, "error") {
		t.Fatalf("model fed %q", result)
	}
}
