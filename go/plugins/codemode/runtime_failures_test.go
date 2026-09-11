package codemode

import (
	"context"
	"os"
	"runtime"
	"strings"
	"testing"
	"time"
)

// Tool group "codemode-runtime": every way the JS runtime itself can fail.
// Each case checks what the model is fed (out + err text) and that the VM
// still runs the NEXT block.

func codemodeRuntimeKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_CODEMODE_RUNTIME") != "1" {
		t.Skip("known bug: " + bug + " (set BOUGH_KNOWN_TOOLS_CODEMODE_RUNTIME=1 to run)")
	}
}

// codemodeRuntimeRun runs code under a hard wall bound so a hang fails
// the test instead of the suite.
func codemodeRuntimeRun(t *testing.T, cm *CodeMode, code string, bound time.Duration) (string, error) {
	t.Helper()
	type res struct {
		out string
		err error
	}
	ch := make(chan res, 1)
	go func() {
		out, err := cm.Run(code)
		ch <- res{out, err}
	}()
	select {
	case r := <-ch:
		return r.out, r.err
	case <-time.After(bound):
		t.Fatalf("block did not return within %v: %s", bound, code)
		return "", nil
	}
}

func codemodeRuntimeNextWorks(t *testing.T, cm *CodeMode) {
	t.Helper()
	out, err := codemodeRuntimeRun(t, cm, `console.log("next ok"); 40+2`, 5*time.Second)
	if err != nil || !strings.Contains(out, "next ok") || !strings.HasSuffix(out, "42") {
		t.Fatalf("VM unusable after failure: out=%q err=%v", out, err)
	}
}

func codemodeRuntimeVM() *CodeMode {
	cm := New(2 * time.Second)
	cm.RegisterTool("bash", func(cmd string) (string, error) { return "ran " + cmd, nil })
	cm.RegisterTool("view", func(path string) (string, error) { return "file " + path, nil })
	return cm
}

func TestCodemodeRuntimeErrors(t *testing.T) {
	cases := []struct {
		name, code string
		wantErr    []string
		wantOut    []string
	}{
		{"syntax error points at line", "const a = 1\nconst b = ;\n", []string{"SyntaxError", "const b = ;", "^"}, nil},
		{"undefined tool", `tools.nope()`, []string{"TypeError", "nope"}, nil},
		{"undefined global", `nope()`, []string{"ReferenceError", "nope is not defined"}, nil},
		{"node global hint", `require("fs")`, []string{"not Node"}, nil},
		{"throw string", `throw "plain string"`, []string{"plain string"}, nil},
		{"throw Error", `throw new Error("an error")`, []string{"an error"}, nil},
		{"throw object", `throw {code: 7, msg: "obj"}`, []string{"object"}, nil},
		{"throw undefined", `throw undefined`, []string{"undefined"}, nil},
		{"throw null", `throw null`, []string{"null"}, nil},
		{"prints then throws", `console.log("PARTIAL"); throw new Error("late")`, []string{"late"}, []string{"PARTIAL"}},
		{"top-level await", `const x = await Promise.resolve(1); console.log(x)`, []string{"SyntaxError"}, nil},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			cm := codemodeRuntimeVM()
			out, err := codemodeRuntimeRun(t, cm, c.code, 20*time.Second)
			if err == nil {
				t.Fatalf("expected error, got out=%q", out)
			}
			for _, w := range c.wantErr {
				if !strings.Contains(err.Error(), w) {
					t.Errorf("err %q lacks %q", err, w)
				}
			}
			for _, w := range c.wantOut {
				if !strings.Contains(out, w) {
					t.Errorf("partial out %q lacks %q", out, w)
				}
			}
			if strings.Contains(err.Error(), "(native)") {
				t.Errorf("native frames leaked: %q", err)
			}
			codemodeRuntimeNextWorks(t, cm)
		})
	}
}

// Wrong argument types reaching a Go tool: goja coerces or throws, it
// must never panic the process.
func TestCodemodeRuntimeWrongArgTypes(t *testing.T) {
	for _, code := range []string{
		`tools.bash(42)`, `tools.view(null)`, `tools.view(undefined)`, `tools.bash({a:1})`,
		`tools.bash()`, `tools.bash(Symbol("s"))`, `tools.bash(1n)`, `tools.bash(() => 1)`,
	} {
		t.Run(code, func(t *testing.T) {
			cm := codemodeRuntimeVM()
			out, err := codemodeRuntimeRun(t, cm, code, 5*time.Second)
			t.Logf("out=%q err=%v", out, err)
			codemodeRuntimeNextWorks(t, cm)
		})
	}
}

// A Go host function that panics must surface as an error, not kill the
// process.
func TestCodemodeRuntimePanickingHostFn(t *testing.T) {
	for name, fn := range map[string]any{
		"string panic": func() (string, error) { panic("host blew up") },
		"nil map":      func() (string, error) { var m map[string]int; m["x"] = 1; return "", nil },
		"error panic":  func() (string, error) { panic(os.ErrClosed) },
	} {
		t.Run(name, func(t *testing.T) {
			cm := codemodeRuntimeVM()
			cm.RegisterTool("boom", fn)
			out, err := codemodeRuntimeRun(t, cm, `console.log("before"); tools.boom()`, 5*time.Second)
			if err == nil {
				t.Fatalf("panic swallowed silently: out=%q", out)
			}
			if !strings.Contains(out, "before") {
				t.Errorf("partial output lost: %q", out)
			}
			t.Logf("err=%v", err)
			codemodeRuntimeNextWorks(t, cm)
		})
	}
}

// Unbounded recursion should be a fast RangeError, not a 30 s spin that
// grows the heap until the timeout interrupts it.
func TestCodemodeRuntimeDeepRecursion(t *testing.T) {
	cm := codemodeRuntimeVM()
	start := time.Now()
	_, err := codemodeRuntimeRun(t, cm, `function f(n){ return f(n+1)+1 }; f(0)`, 20*time.Second)
	if err == nil {
		t.Fatal("unbounded recursion returned no error")
	}
	if !strings.Contains(err.Error(), "RangeError") {
		t.Errorf("want RangeError, got %v (after %s)", err, time.Since(start))
	}
	codemodeRuntimeNextWorks(t, cm)
}

func TestCodemodeRuntimePanicCatchableInJS(t *testing.T) {
	codemodeRuntimeKnown(t, "a panicking Go tool is not a JS exception: RunCtx recovers it as the whole block's error, so try/catch cannot catch it")
	cm := codemodeRuntimeVM()
	cm.RegisterTool("boom", func() (string, error) { panic("host blew up") })
	out, err := codemodeRuntimeRun(t, cm, `try { tools.boom() } catch (e) { console.log("caught") }`, 5*time.Second)
	if err != nil || !strings.Contains(out, "caught") {
		t.Fatalf("out=%q err=%v", out, err)
	}
}

func TestCodemodeRuntimeUnhandledRejection(t *testing.T) {
	cm := codemodeRuntimeVM()
	out, err := codemodeRuntimeRun(t, cm, `Promise.reject(new Error("rejected")); console.log("after")`, 5*time.Second)
	if !strings.Contains(out, "after") {
		t.Fatalf("out=%q err=%v", out, err)
	}
	codemodeRuntimeNextWorks(t, cm)
	// A rejected promise as the completion value must not read as success.
	out, err = codemodeRuntimeRun(t, cm, `Promise.reject(new Error("rejected"))`, 5*time.Second)
	t.Logf("rejected completion: out=%q err=%v", out, err)
	if err == nil && !strings.Contains(out, "rejected") {
		t.Errorf("rejection invisible: out=%q", out)
	}
}

func TestCodemodeRuntimeInfiniteLoopTimeout(t *testing.T) {
	cm := New(300 * time.Millisecond)
	start := time.Now()
	out, err := codemodeRuntimeRun(t, cm, `console.log("spin"); for(;;){}`, 10*time.Second)
	if err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Fatalf("want timeout err, got %v", err)
	}
	if !strings.Contains(out, "spin") {
		t.Errorf("output before spin lost: %q", out)
	}
	if d := time.Since(start); d > 3*time.Second {
		t.Errorf("timeout took %v", d)
	}
	codemodeRuntimeNextWorks(t, cm)
}

func TestCodemodeRuntimeInterruptThenNextBlock(t *testing.T) {
	cm := New(30 * time.Second)
	done := make(chan error, 1)
	go func() { _, err := cm.Run(`while(true){}`); done <- err }()
	time.Sleep(100 * time.Millisecond)
	cm.Interrupt()
	select {
	case err := <-done:
		if err == nil || !strings.Contains(err.Error(), "cancelled") {
			t.Fatalf("err=%v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Interrupt did not stop the block")
	}
	codemodeRuntimeNextWorks(t, cm)
	cm.Interrupt() // between runs: inert
	codemodeRuntimeNextWorks(t, cm)
}

func TestCodemodeRuntimeCtxCancelStopsSpin(t *testing.T) {
	cm := New(30 * time.Second)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { _, err := cm.RunCtx(ctx, `while(true){}`); done <- err }()
	time.Sleep(100 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancel returned nil")
		}
	case <-time.After(3 * time.Second):
		cm.Interrupt()
		<-done
		t.Fatal("ctx cancel did not interrupt a spinning block")
	}
	codemodeRuntimeNextWorks(t, cm)
}

func TestCodemodeRuntimeConsoleLogOddValues(t *testing.T) {
	cases := map[string]string{
		`const o = {a:1}; o.self = o; console.log(o)`: "object",
		`console.log(10n)`:                       "10",
		`console.log(Symbol("sym"))`:             "sym",
		`console.log(function foo(){})`:          "foo",
		`console.log([1, () => 2, 3])`:           "1",
		`console.log(undefined, null, NaN)`:      "undefined null NaN",
		`console.log(new Map([[1,2]]))`:          "",
		`console.log({f: function(){}})`:         "",
		`const a=[1]; a.push(a); console.log(a)`: "",
	}
	for code, want := range cases {
		t.Run(code, func(t *testing.T) {
			cm := codemodeRuntimeVM()
			out, err := codemodeRuntimeRun(t, cm, code, 5*time.Second)
			if err != nil {
				t.Fatalf("console.log threw: %v", err)
			}
			if !strings.Contains(out, want) {
				t.Errorf("out %q lacks %q", out, want)
			}
			codemodeRuntimeNextWorks(t, cm)
		})
	}
}

func TestCodemodeRuntimeReturnValueNoLog(t *testing.T) {
	cm := codemodeRuntimeVM()
	out, err := cm.Run(`const x = {a: 1}; x`)
	if err != nil {
		t.Fatal(err)
	}
	if out == "[object Object]" {
		t.Errorf("object completion value unreadable: %q", out)
	}
}

// Memory soak: behind BOUGH_SOAK_CODEMODE_RUNTIME.
func TestCodemodeRuntimeHugeAllocation(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_CODEMODE_RUNTIME") != "1" {
		t.Skip("set BOUGH_SOAK_CODEMODE_RUNTIME=1 for the memory soak")
	}
	cm := New(5 * time.Second)
	out, err := codemodeRuntimeRun(t, cm, `let s = "x"; for (let i=0;i<30;i++) s += s; s.length`, 30*time.Second)
	t.Logf("huge string: out=%.80q err=%v", out, err)
	out, err = codemodeRuntimeRun(t, cm, `const a = []; while (true) a.push(new Array(1e4).fill(1));`, 30*time.Second)
	t.Logf("unbounded array: out=%.80q err=%v", out, err)
	var ms runtime.MemStats
	runtime.ReadMemStats(&ms)
	t.Logf("heap after: %d MB", ms.HeapAlloc>>20)
	codemodeRuntimeNextWorks(t, cm)
}

// codemode returns output whole; capping is the loop's job.
func TestCodemodeRuntimeLargeOutput(t *testing.T) {
	cm := codemodeRuntimeVM()
	out, err := cm.Run(`for (let i=0;i<20000;i++) console.log("line " + i)`)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "line 19999") || !strings.HasPrefix(out, "line 0\n") {
		t.Fatalf("large output truncated: len=%d", len(out))
	}
}
