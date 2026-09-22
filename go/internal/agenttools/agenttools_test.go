package agenttools

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func noop(context.Context, Call) (Result, error) { return Result{}, nil }

func TestRegistryRegisterLookupSorted(t *testing.T) {
	t.Parallel()
	r := NewRegistry()
	for _, n := range []string{"write", "bash", "view"} {
		if _, err := r.Register(Tool{Name: n, Call: noop}); err != nil {
			t.Fatalf("register %s: %v", n, err)
		}
	}
	var names []string
	for _, tl := range r.Tools() {
		names = append(names, tl.Name)
	}
	if got := strings.Join(names, ","); got != "bash,view,write" {
		t.Fatalf("Tools() = %s, want sorted bash,view,write", got)
	}
	if _, ok := r.Lookup("view"); !ok {
		t.Fatal("Lookup(view) missing")
	}
	if _, ok := r.Lookup("nope"); ok {
		t.Fatal("Lookup(nope) found")
	}
}

func TestRegistryClashAndInvalidName(t *testing.T) {
	t.Parallel()
	r := NewRegistry()
	if _, err := r.Register(Tool{Name: "bash", Call: noop}); err != nil {
		t.Fatal(err)
	}
	_, err := r.Register(Tool{Name: "bash", Call: noop})
	if err == nil || !strings.Contains(err.Error(), `"bash"`) {
		t.Fatalf("clash error = %v, want one naming bash", err)
	}
	for _, bad := range []string{"", "has space", "dot.ted", "tools.bash", strings.Repeat("a", 65), "ünï"} {
		_, err := r.Register(Tool{Name: bad, Call: noop})
		if err == nil {
			t.Fatalf("Register(%q) accepted an invalid name", bad)
		}
	}
	if _, err := r.Register(Tool{Name: "nocall"}); err == nil {
		t.Fatal("Register accepted a tool with no Call")
	}
	if !ValidName(strings.Repeat("a", 64)) || !ValidName("mcp__srv__do-it") {
		t.Fatal("ValidName rejected a valid name")
	}
}

func TestRegistryUnregisterAndChanged(t *testing.T) {
	t.Parallel()
	r := NewRegistry()
	ch := r.Changed()
	un, err := r.Register(Tool{Name: "bash", Call: noop})
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-ch:
	default:
		t.Fatal("Changed not closed by Register")
	}
	ch = r.Changed()
	select {
	case <-ch:
		t.Fatal("fresh Changed already closed")
	default:
	}
	un()
	select {
	case <-ch:
	default:
		t.Fatal("Changed not closed by unregister")
	}
	if _, ok := r.Lookup("bash"); ok {
		t.Fatal("bash still registered")
	}
	// A stale unregister must not remove a later registration of the name.
	if _, err := r.Register(Tool{Name: "bash", Call: noop}); err != nil {
		t.Fatal(err)
	}
	ch = r.Changed()
	un()
	if _, ok := r.Lookup("bash"); !ok {
		t.Fatal("stale unregister removed the new bash")
	}
	select {
	case <-ch:
		t.Fatal("stale unregister fired Changed")
	default:
	}
}

func TestDecodeNamesTheTool(t *testing.T) {
	t.Parallel()
	var v struct{ Path string }
	if err := Decode("view", nil, &v); err != nil {
		t.Fatalf("empty args: %v", err)
	}
	err := Decode("view", json.RawMessage(`{"path": 3}`), &v)
	if err == nil || !strings.HasPrefix(err.Error(), "view: arguments:") {
		t.Fatalf("Decode error = %v", err)
	}
	s := Object([]string{"path"}, map[string]any{"path": Prop("string", "file")})
	if s["type"] != "object" || s["required"].([]any)[0] != "path" {
		t.Fatalf("Object = %v", s)
	}
}

func TestCallEmitNilSafe(t *testing.T) {
	t.Parallel()
	Call{}.Emit("x")
	var got string
	Call{Progress: func(s string) { got += s }}.Emit("hi")
	if got != "hi" {
		t.Fatalf("Emit = %q", got)
	}
}

func TestRegisterAllUndoesOnClash(t *testing.T) {
	t.Parallel()
	r := NewRegistry()
	if _, err := r.Register(Tool{Name: "patch", Call: noop}); err != nil {
		t.Fatal(err)
	}
	if _, err := RegisterAll(r, Tool{Name: "bash", Call: noop}, Tool{Name: "patch", Call: noop}); err == nil || !strings.Contains(err.Error(), `"patch"`) {
		t.Fatalf("clash err = %v", err)
	}
	if _, ok := r.Lookup("bash"); ok {
		t.Fatal("a failed RegisterAll left bash registered")
	}
	off, err := RegisterAll(r, Tool{Name: "bash", Call: noop}, Tool{Name: "view", Call: noop})
	if err != nil {
		t.Fatal(err)
	}
	off()
	if got := len(r.Tools()); got != 1 {
		t.Fatalf("after unregister, %d tools remain, want only patch", got)
	}
}
