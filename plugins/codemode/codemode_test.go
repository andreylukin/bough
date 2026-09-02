package codemode

import (
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
	if _, err := cm.Run(`var counter = 41`); err != nil {
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
