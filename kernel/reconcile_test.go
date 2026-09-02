package kernel

import (
	"fmt"
	"strings"
	"testing"
)

// recPlugin logs apply/dispose per row and provides keys whose value
// embeds the row's "v" config, so tests can see live values change.
type recPlugin struct {
	name    string
	inject  []string
	provide []string
	log     *[]string
}

func (p *recPlugin) Name() string     { return p.name }
func (p *recPlugin) Inject() []string { return p.inject }
func (p *recPlugin) Apply(ctx *Context, cfg map[string]any) error {
	*p.log = append(*p.log, "apply:"+p.name)
	for _, k := range p.provide {
		ctx.Provide(k, fmt.Sprintf("%s:%v", p.name, cfg["v"]))
	}
	name := p.name
	ctx.Effect(func() { *p.log = append(*p.log, "dispose:"+name) })
	return nil
}

func count(log []string, entry string) int {
	n := 0
	for _, e := range log {
		if e == entry {
			n++
		}
	}
	return n
}

// Two independent rows; changing one row's config remounts exactly it.
func TestReconcileConfigChangeRemountsOnlyThatRow(t *testing.T) {
	var log []string
	Register("rec-a", func() Plugin { return &recPlugin{name: "rec-a", provide: []string{"rec-svc-a"}, log: &log} })
	Register("rec-b", func() Plugin { return &recPlugin{name: "rec-b", provide: []string{"rec-svc-b"}, log: &log} })
	c := NewContext()
	rows := []Row{
		{ID: "a", Plugin: "rec-a", Config: map[string]any{"v": 1}},
		{ID: "b", Plugin: "rec-b", Config: map[string]any{"v": 1}},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	log = nil

	newRows := []Row{
		{ID: "a", Plugin: "rec-a", Config: map[string]any{"v": 1}},
		{ID: "b", Plugin: "rec-b", Config: map[string]any{"v": 2}},
	}
	if err := c.Reconcile(newRows); err != nil {
		t.Fatal(err)
	}
	if count(log, "dispose:rec-b") != 1 || count(log, "apply:rec-b") != 1 {
		t.Fatalf("b not remounted exactly once: %v", log)
	}
	if count(log, "dispose:rec-a") != 0 || count(log, "apply:rec-a") != 0 {
		t.Fatalf("a should be untouched: %v", log)
	}
	v, err := Get[string](c, "rec-svc-b")
	if err != nil || v != "rec-b:2" {
		t.Fatalf("rec-svc-b = %q, %v", v, err)
	}
}

// Dependent closure: changing a provider row remounts its dependents.
func TestReconcileDependentClosure(t *testing.T) {
	var log []string
	Register("rec-prov", func() Plugin { return &recPlugin{name: "rec-prov", provide: []string{"rec-svc-p"}, log: &log} })
	Register("rec-dep", func() Plugin {
		return &recPlugin{name: "rec-dep", inject: []string{"rec-svc-p"}, provide: []string{"rec-svc-d"}, log: &log}
	})
	c := NewContext()
	rows := []Row{
		{ID: "p", Plugin: "rec-prov", Config: map[string]any{"v": 1}},
		{ID: "d", Plugin: "rec-dep", Config: map[string]any{"v": 1}},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	log = nil

	rows[0].Config = map[string]any{"v": 2}
	if err := c.Reconcile(rows); err != nil {
		t.Fatal(err)
	}
	want := []string{"dispose:rec-dep", "dispose:rec-prov", "apply:rec-prov", "apply:rec-dep"}
	if strings.Join(log, ",") != strings.Join(want, ",") {
		t.Fatalf("log = %v, want %v", log, want)
	}
	v, err := Get[string](c, "rec-svc-p")
	if err != nil || v != "rec-prov:2" {
		t.Fatalf("rec-svc-p = %q, %v", v, err)
	}
}

// Removing a row disposes its effects and withdraws its services.
func TestReconcileRemovedRowDisposed(t *testing.T) {
	var log []string
	Register("rec-gone", func() Plugin { return &recPlugin{name: "rec-gone", provide: []string{"rec-svc-gone"}, log: &log} })
	c := NewContext()
	if err := c.Mount([]Row{{ID: "g", Plugin: "rec-gone"}}); err != nil {
		t.Fatal(err)
	}
	if err := c.Reconcile(nil); err != nil {
		t.Fatal(err)
	}
	if count(log, "dispose:rec-gone") != 1 {
		t.Fatalf("effects not disposed: %v", log)
	}
	if _, err := Get[string](c, "rec-svc-gone"); err == nil {
		t.Fatal("service should be withdrawn")
	}
}

// A bad candidate leaves the old tree serving.
func TestReconcileBadCandidateKeepsOldTree(t *testing.T) {
	var log []string
	Register("rec-good", func() Plugin { return &recPlugin{name: "rec-good", provide: []string{"rec-svc-good"}, log: &log} })
	Register("rec-stuck", func() Plugin {
		return &recPlugin{name: "rec-stuck", inject: []string{"rec-never-provided"}, log: &log}
	})
	c := NewContext()
	good := []Row{{ID: "g", Plugin: "rec-good", Config: map[string]any{"v": 1}}}
	if err := c.Mount(good); err != nil {
		t.Fatal(err)
	}

	// Unknown plugin: rejected before anything unmounts.
	log = nil
	err := c.Reconcile([]Row{{ID: "g", Plugin: "rec-good", Config: map[string]any{"v": 1}}, {ID: "x", Plugin: "no-such"}})
	if err == nil || !strings.Contains(err.Error(), "no-such") {
		t.Fatalf("want unknown-plugin error, got %v", err)
	}
	if len(log) != 0 {
		t.Fatalf("tree was touched: %v", log)
	}

	// Stuck dependencies: restore remounts the previous rows.
	err = c.Reconcile([]Row{{ID: "g", Plugin: "rec-good", Config: map[string]any{"v": 2}}, {ID: "s", Plugin: "rec-stuck"}})
	if err == nil || !strings.Contains(err.Error(), "restored") {
		t.Fatalf("want restored error, got %v", err)
	}
	v, gerr := Get[string](c, "rec-svc-good")
	if gerr != nil || v != "rec-good:1" {
		t.Fatalf("old tree not serving: %q, %v", v, gerr)
	}
}
