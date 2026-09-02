package kernel

import (
	"strings"
	"testing"
)

type testPlugin struct {
	name    string
	inject  []string
	provide []string
	applied *[]string // shared log of apply order
}

func (p *testPlugin) Name() string     { return p.name }
func (p *testPlugin) Inject() []string { return p.inject }
func (p *testPlugin) Apply(ctx *Context, cfg map[string]any) error {
	*p.applied = append(*p.applied, p.name)
	for _, k := range p.provide {
		ctx.Provide(k, p.name)
	}
	return nil
}

func TestProvideGet(t *testing.T) {
	c := NewContext()
	c.Provide("n", 42)
	got, err := Get[int](c, "n")
	if err != nil || got != 42 {
		t.Fatalf("Get = %v, %v", got, err)
	}
	if _, err := Get[int](c, "absent"); err == nil {
		t.Fatal("want error for absent key")
	}
	if _, err := Get[string](c, "n"); err == nil {
		t.Fatal("want error for wrong type")
	}
}

func TestInjectGatedOrdering(t *testing.T) {
	var log []string
	Register("test-b", func() Plugin {
		return &testPlugin{name: "test-b", inject: []string{"svc-a"}, applied: &log}
	})
	Register("test-a", func() Plugin {
		return &testPlugin{name: "test-a", provide: []string{"svc-a"}, applied: &log}
	})
	c := NewContext()
	// b listed first; must still mount after a.
	rows := []Row{
		{ID: "b", Plugin: "test-b"},
		{ID: "a", Plugin: "test-a"},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	if len(log) != 2 || log[0] != "test-a" || log[1] != "test-b" {
		t.Fatalf("apply order = %v", log)
	}
}

func TestFailLoudMissingKey(t *testing.T) {
	var log []string
	Register("test-needy", func() Plugin {
		return &testPlugin{name: "test-needy", inject: []string{"nope"}, applied: &log}
	})
	c := NewContext()
	err := c.Mount([]Row{{ID: "needy", Plugin: "test-needy"}})
	if err == nil {
		t.Fatal("want error")
	}
	if !strings.Contains(err.Error(), `"needy"`) || !strings.Contains(err.Error(), "nope") {
		t.Fatalf("error should name row and key: %v", err)
	}
}

func TestUnknownPlugin(t *testing.T) {
	c := NewContext()
	err := c.Mount([]Row{{ID: "x", Plugin: "no-such-plugin"}})
	if err == nil || !strings.Contains(err.Error(), "no-such-plugin") {
		t.Fatalf("want unknown-plugin error, got %v", err)
	}
}

func TestEffectLIFO(t *testing.T) {
	c := NewContext()
	var order []int
	c.Effect(func() { order = append(order, 1) })
	c.Effect(func() { order = append(order, 2) })
	c.Effect(func() { order = append(order, 3) })
	c.Unmount()
	if len(order) != 3 || order[0] != 3 || order[1] != 2 || order[2] != 1 {
		t.Fatalf("disposal order = %v, want [3 2 1]", order)
	}
}

func TestEmitContainedPanic(t *testing.T) {
	c := NewContext()
	var ran bool
	c.On("ev", func(any) { panic("boom") })
	c.On("ev", func(any) { ran = true })
	c.Emit("ev", nil) // must not panic
	if !ran {
		t.Fatal("second listener did not run after first panicked")
	}
}

func TestOnDisposer(t *testing.T) {
	c := NewContext()
	var n int
	off := c.On("ev", func(any) { n++ })
	c.Emit("ev", nil)
	off()
	c.Emit("ev", nil)
	if n != 1 {
		t.Fatalf("n = %d, want 1", n)
	}
}
