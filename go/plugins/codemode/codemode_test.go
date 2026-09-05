package codemode

import (
	"context"
	"strings"
	"testing"
	"time"
)

func TestRunToolAndConsole(t *testing.T) {
	cm := New(5 * time.Second)
	cm.RegisterTool("greet", func(name string) (string, error) {
		return "hello " + name, nil
	})
	out, err := cm.Run(`console.log(tools.greet("world")); tools.greet("again")`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if !strings.Contains(out, "hello world\n") {
		t.Errorf("console output missing: %q", out)
	}
	if !strings.HasSuffix(out, "hello again") {
		t.Errorf("final value missing: %q", out)
	}
}

func TestToolErrorBecomesException(t *testing.T) {
	cm := New(5 * time.Second)
	cm.RegisterTool("boom", func() (string, error) {
		return "", &testErr{}
	})
	out, err := cm.Run(`try { tools.boom() } catch (e) { console.log("caught: " + e) } "ok"`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if !strings.Contains(out, "caught:") {
		t.Errorf("error not thrown as JS exception: %q", out)
	}
}

type testErr struct{}

func (*testErr) Error() string { return "kaboom" }

func TestInterruptInfiniteLoop(t *testing.T) {
	cm := New(100 * time.Millisecond)
	done := make(chan struct{})
	var err error
	go func() {
		_, err = cm.Run(`while (true) {}`)
		close(done)
	}()
	select {
	case <-done:
		if err == nil {
			t.Fatal("expected interrupt error, got nil")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("interrupt did not fire")
	}

	// VM is reusable after an interrupt.
	out, err := cm.Run(`1 + 1`)
	if err != nil || out != "2" {
		t.Fatalf("VM not reusable after interrupt: out=%q err=%v", out, err)
	}
}

func TestRunHookReturnsObjectAndSeesTools(t *testing.T) {
	cm := New(5 * time.Second)
	cm.RegisterTool("greet", func(name string) (string, error) {
		return "hello " + name, nil
	})
	res, err := cm.RunHook(`return {who: tools.greet(event.input), echo: event.input}`,
		map[string]any{"input": "world"})
	if err != nil {
		t.Fatalf("RunHook: %v", err)
	}
	if res["who"] != "hello world" || res["echo"] != "world" {
		t.Fatalf("res = %v", res)
	}
}

func TestRunHookNoReturnIsNil(t *testing.T) {
	cm := New(5 * time.Second)
	res, err := cm.RunHook(`var x = event`, map[string]any{})
	if err != nil {
		t.Fatalf("RunHook: %v", err)
	}
	if res != nil {
		t.Fatalf("res = %v, want nil", res)
	}
}

func TestRunHookExceptionIsError(t *testing.T) {
	cm := New(5 * time.Second)
	if _, err := cm.RunHook(`throw new Error("bad hook")`, map[string]any{}); err == nil {
		t.Fatal("expected error from throwing hook")
	}
}

func TestRunHookSharesVMGlobals(t *testing.T) {
	cm := New(5 * time.Second)
	// Declarations are block-scoped since blocks run in their own
	// function scope; an undeclared assignment is the shared global.
	if _, err := cm.Run(`counter = 41`); err != nil {
		t.Fatalf("Run: %v", err)
	}
	res, err := cm.RunHook(`counter++; return {n: counter}`, map[string]any{})
	if err != nil {
		t.Fatalf("RunHook: %v", err)
	}
	if res["n"] != int64(42) {
		t.Fatalf("res = %v (%T), want 42", res["n"], res["n"])
	}
}

func TestUndefinedResultOmitted(t *testing.T) {
	cm := New(5 * time.Second)
	out, err := cm.Run(`var x = 1;`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if out != "" {
		t.Errorf("expected empty output, got %q", out)
	}
}

func TestBlocksDoNotLeakDeclarations(t *testing.T) {
	cm := New(5 * time.Second)
	blocks := []string{"const x = 1; let y = 2; var z = 3; x + y + z", "const x = 10; let y = 20; var z = 30; x + y + z"}
	for i, want := range []string{"6", "60"} {
		out, err := cm.Run(blocks[i])
		if err != nil {
			t.Fatalf("block %d: %v", i, err)
		}
		if out != want {
			t.Errorf("block %d = %q, want %q", i, out, want)
		}
	}
	// Undeclared assignment is still a shared global; tools stays reachable.
	cm.RegisterTool("id", func(s string) (string, error) { return s, nil })
	if _, err := cm.Run(`shared = tools.id("g")`); err != nil {
		t.Fatal(err)
	}
	out, err := cm.Run(`shared + tools.id("!")`)
	if err != nil || out != "g!" {
		t.Errorf("globals: %q %v", out, err)
	}
}

func TestToolErrorStripsNativeFrame(t *testing.T) {
	cm := New(5 * time.Second)
	cm.RegisterTool("boom", func() (string, error) { return "", &testErr{} })
	_, err := cm.Run(`tools.boom()`)
	if err == nil {
		t.Fatal("want error")
	}
	if strings.Contains(err.Error(), "(native)") {
		t.Errorf("native frame leaked: %q", err.Error())
	}
	if !strings.Contains(err.Error(), "kaboom") {
		t.Errorf("message lost: %q", err.Error())
	}
}

// RunContext is the innermost run's context while a script runs and
// Background between runs, so a host call can tell a live turn from
// idle and nested runs restore the outer one.
func TestRunContextFollowsTheRun(t *testing.T) {
	cm := New(2 * time.Second)
	if cm.RunContext() != context.Background() {
		t.Fatal("idle: Background")
	}
	type key struct{}
	outer := context.WithValue(context.Background(), key{}, "outer")
	var seen []any
	cm.RegisterTool("peek", func() string {
		seen = append(seen, cm.RunContext().Value(key{}))
		return ""
	})
	cm.RegisterTool("nested", func() string {
		inner := context.WithValue(context.Background(), key{}, "inner")
		_, _ = cm.RunCtx(inner, "tools.peek()")
		return ""
	})
	if _, err := cm.RunCtx(outer, "tools.peek(); tools.nested(); tools.peek()"); err != nil {
		t.Fatal(err)
	}
	if len(seen) != 3 || seen[0] != "outer" || seen[1] != "inner" || seen[2] != "outer" {
		t.Fatalf("contexts seen: %v", seen)
	}
	if cm.RunContext() != context.Background() {
		t.Fatal("after the run: Background again")
	}
}

// A syntax error points at the line it failed on: goja reports only
// "Line L:C", which leaves the author guessing.
func TestSyntaxErrorShowsTheOffendingLine(t *testing.T) {
	cm := New(time.Second)
	_, err := cm.Run("var a = 1\nvar b = 2\nconsole.log(a b c)\n")
	if err == nil {
		t.Fatal("want a syntax error")
	}
	if !strings.Contains(err.Error(), "console.log(a b c)") || !strings.Contains(err.Error(), "^") {
		t.Fatalf("the error must quote the line and point at the column:\n%v", err)
	}
}

// A model that mistakes this runtime for Node gets told what to use
// instead: "require is not defined" alone does not say why.
func TestNodeGlobalsExplainThemselves(t *testing.T) {
	cm := New(2 * time.Second)
	for _, tc := range []struct{ code, want string }{
		{`require("fs")`, "not Node"},
		{`Buffer.from("x")`, "tools.write"},
		{`fetch("http://x")`, "curl"},
		{`process.exit(1)`, "tools.bash"},
	} {
		_, err := cm.Run(tc.code)
		if err == nil {
			t.Fatalf("%s did not error", tc.code)
		}
		if !strings.Contains(err.Error(), tc.want) {
			t.Fatalf("%s -> %v, want a hint mentioning %q", tc.code, err, tc.want)
		}
	}
	// An ordinary undefined name keeps its plain message.
	if _, err := cm.Run(`somethingElse()`); err == nil || strings.Contains(err.Error(), "not Node") {
		t.Fatalf("plain ReferenceError = %v", err)
	}
}

// console.log has to render what the tools return. goja's String() on
// an object is "[object Object]", so a model printing a structured
// result used to get nothing it could read — seen in a real run as
// "[object Object],[object Object],[object Object],…".
func TestConsoleLogRendersObjects(t *testing.T) {
	cm := New(2 * time.Second)
	out, err := cm.Run(`console.log({name: "a", n: 2})`)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, "[object Object]") {
		t.Fatalf("an object should render as JSON, got %q", out)
	}
	for _, want := range []string{`"name": "a"`, `"n": 2`} {
		if !strings.Contains(out, want) {
			t.Errorf("want %s in:\n%s", want, out)
		}
	}
}

func TestConsoleLogRendersArraysOfObjects(t *testing.T) {
	cm := New(2 * time.Second)
	out, err := cm.Run(`console.log([{id: 1}, {id: 2}])`)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, "[object Object]") {
		t.Fatalf("an array of objects should render as JSON, got %q", out)
	}
	if !strings.Contains(out, `"id": 1`) || !strings.Contains(out, `"id": 2`) {
		t.Errorf("both elements should be readable:\n%s", out)
	}
}

// Primitives are unchanged: a logged string stays bare text, not a
// quoted JSON string.
func TestConsoleLogLeavesPrimitivesAlone(t *testing.T) {
	cm := New(2 * time.Second)
	for code, want := range map[string]string{
		`console.log("hello")`:      "hello\n",
		`console.log(42)`:           "42\n",
		`console.log(true)`:         "true\n",
		`console.log(null)`:         "null\n",
		`console.log("a", 1, true)`: "a 1 true\n",
	} {
		out, err := cm.Run(code)
		if err != nil {
			t.Fatalf("%s: %v", code, err)
		}
		if out != want {
			t.Errorf("%s = %q, want %q", code, out, want)
		}
	}
}

// A value JSON cannot represent must not lose the log line.
func TestConsoleLogFallsBackOnUnmarshalable(t *testing.T) {
	cm := New(2 * time.Second)
	out, err := cm.Run(`var a = {}; a.self = a; console.log(a)`)
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(out) == "" {
		t.Error("a cyclic object should still print something")
	}
}

// A tool written in Go returns its own types — map[string]int, a slice
// of structs — and goja exports them unchanged. Matching only
// map[string]any left every one of those printing "[object Object]",
// which is exactly the case this was written for.
func TestConsoleLogRendersGoTypedResults(t *testing.T) {
	cm := New(2 * time.Second)
	cm.RegisterTool("counts", func(string) map[string]int { return map[string]int{"a": 2} })
	cm.RegisterTool("names", func() []string { return []string{"x", "y"} })
	cm.RegisterTool("pair", func() struct {
		Name string `json:"name"`
		N    int    `json:"n"`
	} {
		return struct {
			Name string `json:"name"`
			N    int    `json:"n"`
		}{"a", 1}
	})

	for code, want := range map[string]string{
		`console.log(tools.counts("x"))`: `"a": 2`,
		`console.log(tools.names())`:     `"x"`,
		`console.log(tools.pair())`:      `"name": "a"`,
	} {
		out, err := cm.Run(code)
		if err != nil {
			t.Fatalf("%s: %v", code, err)
		}
		if strings.Contains(out, "[object Object]") {
			t.Errorf("%s printed [object Object]:\n%s", code, out)
		}
		if !strings.Contains(out, want) {
			t.Errorf("%s should contain %s, got:\n%s", code, want, out)
		}
	}
}

// Command output arrives as bytes and is text, not a structure to
// serialise.
func TestConsoleLogRendersBytesAsText(t *testing.T) {
	cm := New(2 * time.Second)
	cm.RegisterTool("raw", func() []byte { return []byte("hello from a command") })
	out, err := cm.Run(`console.log(tools.raw())`)
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(out) != "hello from a command" {
		t.Errorf("bytes should print as text, got %q", out)
	}
}
