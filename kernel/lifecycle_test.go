package kernel

import (
	"fmt"
	"testing"
)

// lcPlugin exercises the live lifecycle: declared injects, optional
// Gets (tolerated when absent, logged either way), provides embedding
// the row's "v" config, and an optional Apply failure trigger.
type lcPlugin struct {
	name    string
	inject  []string
	provide []string
	optGet  []string // keys Get at Apply time, miss tolerated
	failOn  any      // Apply errors when cfg["v"] == failOn
	log     *[]string
}

func (p *lcPlugin) Name() string     { return p.name }
func (p *lcPlugin) Inject() []string { return p.inject }
func (p *lcPlugin) Apply(ctx *Context, cfg map[string]any) error {
	if p.failOn != nil && cfg["v"] == p.failOn {
		*p.log = append(*p.log, "fail:"+p.name)
		return fmt.Errorf("%s: configured to fail", p.name)
	}
	*p.log = append(*p.log, "apply:"+p.name)
	for _, k := range p.optGet {
		if v, err := Get[string](ctx, k); err == nil {
			*p.log = append(*p.log, fmt.Sprintf("opt:%s:%s=%s", p.name, k, v))
		} else {
			*p.log = append(*p.log, fmt.Sprintf("opt:%s:%s=miss", p.name, k))
		}
	}
	for _, k := range p.provide {
		ctx.Provide(k, fmt.Sprintf("%s:%v", p.name, cfg["v"]))
	}
	name := p.name
	ctx.Effect(func() { *p.log = append(*p.log, "dispose:"+name) })
	return nil
}

func last(log []string, prefix string) string {
	out := ""
	for _, e := range log {
		if len(e) >= len(prefix) && e[:len(prefix)] == prefix {
			out = e
		}
	}
	return out
}

func stateOf(c *Context, id string) RowStatus {
	for _, rs := range c.Rows() {
		if rs.ID == id {
			return rs
		}
	}
	return RowStatus{}
}

// A row that Gets a key optionally (miss tolerated) reloads when a
// provider for that key lands later, and again when it is withdrawn.
func TestOptionalProviderTriggersReload(t *testing.T) {
	var log []string
	Register("lc-loop", func() Plugin { return &lcPlugin{name: "lc-loop", optGet: []string{"lc-hist"}, log: &log} })
	Register("lc-hist", func() Plugin { return &lcPlugin{name: "lc-hist", provide: []string{"lc-hist"}, log: &log} })
	c := NewContext()
	loopOnly := []Row{{ID: "loop", Plugin: "lc-loop"}}
	if err := c.Mount(loopOnly); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-loop:"); got != "opt:lc-loop:lc-hist=miss" {
		t.Fatalf("boot without provider: %q", got)
	}

	// Provider lands via reconcile: loop reloads and sees it.
	both := []Row{{ID: "loop", Plugin: "lc-loop"}, {ID: "hist", Plugin: "lc-hist", Config: map[string]any{"v": 1}}}
	if err := c.Reconcile(both); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-loop:"); got != "opt:lc-loop:lc-hist=lc-hist:1" {
		t.Fatalf("after provider mounted: %q (log %v)", got, log)
	}

	// Provider withdrawn: loop reloads and falls back.
	if err := c.Reconcile(loopOnly); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-loop:"); got != "opt:lc-loop:lc-hist=miss" {
		t.Fatalf("after provider withdrawn: %q (log %v)", got, log)
	}
	if count(log, "apply:lc-loop") != 3 {
		t.Fatalf("loop should have applied exactly 3 times: %v", log)
	}
}

// At boot, a consumer listed above its optional provider self-heals:
// the settle pass after the strict mount reloads it.
func TestMountSelfHealsOptionalOrder(t *testing.T) {
	var log []string
	Register("lc-loop2", func() Plugin { return &lcPlugin{name: "lc-loop2", optGet: []string{"lc-hist2"}, log: &log} })
	Register("lc-hist2", func() Plugin { return &lcPlugin{name: "lc-hist2", provide: []string{"lc-hist2"}, log: &log} })
	c := NewContext()
	rows := []Row{
		{ID: "loop", Plugin: "lc-loop2"}, // listed before its optional provider
		{ID: "hist", Plugin: "lc-hist2", Config: map[string]any{"v": 1}},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-loop2:"); got != "opt:lc-loop2:lc-hist2=lc-hist2:1" {
		t.Fatalf("loop did not self-heal at boot: %q (log %v)", got, log)
	}
}

// A row whose Apply errors goes Failed, is isolated (others keep
// going), is never retried on an identical reconcile, and is retried
// when its spec changes.
func TestFailedIsolationAndRetry(t *testing.T) {
	var log []string
	Register("lc-ok", func() Plugin { return &lcPlugin{name: "lc-ok", provide: []string{"lc-ok"}, log: &log} })
	Register("lc-bad", func() Plugin { return &lcPlugin{name: "lc-bad", failOn: "boom", log: &log} })
	c := NewContext()
	if err := c.Mount([]Row{{ID: "ok", Plugin: "lc-ok", Config: map[string]any{"v": 1}}}); err != nil {
		t.Fatal(err)
	}

	bad := []Row{
		{ID: "ok", Plugin: "lc-ok", Config: map[string]any{"v": 1}},
		{ID: "bad", Plugin: "lc-bad", Config: map[string]any{"v": "boom"}},
	}
	if err := c.Reconcile(bad); err != nil {
		t.Fatal(err)
	}
	if rs := stateOf(c, "bad"); rs.State != StateFailed || rs.Err == nil {
		t.Fatalf("bad row = %+v", rs)
	}
	if rs := stateOf(c, "ok"); rs.State != StateActive {
		t.Fatalf("ok row should be isolated from the failure: %+v", rs)
	}
	if count(log, "fail:lc-bad") != 1 {
		t.Fatalf("bad applied != 1: %v", log)
	}

	// Identical reconcile: no retry loop.
	if err := c.Reconcile(bad); err != nil {
		t.Fatal(err)
	}
	if count(log, "fail:lc-bad") != 1 {
		t.Fatalf("failed row was retried without a spec change: %v", log)
	}

	// Spec change: retried, mounts.
	bad[1].Config = map[string]any{"v": "fine"}
	if err := c.Reconcile(bad); err != nil {
		t.Fatal(err)
	}
	if rs := stateOf(c, "bad"); rs.State != StateActive {
		t.Fatalf("bad row after fix = %+v", rs)
	}
}

// Providing on an already-provided key: last write wins and dependents
// of the key reload to see the new value.
func TestLastWriteWinsReloadsDependents(t *testing.T) {
	var log []string
	Register("lc-p1", func() Plugin { return &lcPlugin{name: "lc-p1", provide: []string{"lc-dup"}, log: &log} })
	Register("lc-p2", func() Plugin { return &lcPlugin{name: "lc-p2", provide: []string{"lc-dup"}, log: &log} })
	Register("lc-dep", func() Plugin {
		return &lcPlugin{name: "lc-dep", inject: []string{"lc-dup"}, optGet: []string{"lc-dup"}, log: &log}
	})
	c := NewContext()
	rows := []Row{
		{ID: "p1", Plugin: "lc-p1", Config: map[string]any{"v": 1}},
		{ID: "d", Plugin: "lc-dep"},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-dep:"); got != "opt:lc-dep:lc-dup=lc-p1:1" {
		t.Fatalf("dep sees %q", got)
	}

	rows = append(rows, Row{ID: "p2", Plugin: "lc-p2", Config: map[string]any{"v": 2}})
	if err := c.Reconcile(rows); err != nil {
		t.Fatal(err)
	}
	if got := last(log, "opt:lc-dep:"); got != "opt:lc-dep:lc-dup=lc-p2:2" {
		t.Fatalf("dep did not reload onto the last writer: %q (log %v)", got, log)
	}
	if count(log, "apply:lc-p1") != 1 {
		t.Fatalf("p1 should not reload (it is the overwritten provider): %v", log)
	}
}

// Rows() reports the desired set with live states in config order.
func TestRowsStates(t *testing.T) {
	var log []string
	Register("lc-a", func() Plugin { return &lcPlugin{name: "lc-a", provide: []string{"lc-a"}, log: &log} })
	Register("lc-needy", func() Plugin { return &lcPlugin{name: "lc-needy", inject: []string{"lc-nope"}, log: &log} })
	Register("lc-boom", func() Plugin { return &lcPlugin{name: "lc-boom", failOn: "x", log: &log} })
	c := NewContext()
	rows := []Row{
		{ID: "a", Plugin: "lc-a"},
		{ID: "off", Plugin: "lc-a", Disabled: true},
		{ID: "needy", Plugin: "lc-needy"},
		{ID: "boom", Plugin: "lc-boom", Config: map[string]any{"v": "x"}},
	}
	if err := c.Reconcile(rows); err != nil { // tolerant mount from empty
		t.Fatal(err)
	}
	got := c.Rows()
	if len(got) != 4 {
		t.Fatalf("Rows() = %+v", got)
	}
	want := []struct {
		id    string
		state State
	}{{"a", StateActive}, {"off", StateDisabled}, {"needy", StatePending}, {"boom", StateFailed}}
	for i, w := range want {
		if got[i].ID != w.id || got[i].State != w.state {
			t.Fatalf("Rows()[%d] = %+v, want %s/%s", i, got[i], w.id, w.state)
		}
	}
	if len(got[2].Missing) != 1 || got[2].Missing[0] != "lc-nope" {
		t.Fatalf("pending missing = %v", got[2].Missing)
	}
	if got[3].Err == nil {
		t.Fatal("failed row should carry its error")
	}
}
