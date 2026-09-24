package orb

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/tools"
)

// Unmount gives /orb back only when a setup skill held it before: a
// session with the skill turned off must not gain a phantom /orb.
func TestOrbCommandRestoresOnlyExistingSkill(t *testing.T) {
	t.Parallel()
	for _, had := range []bool{false, true} {
		reg := commands.NewRegistry()
		if had {
			reg.Register(commands.CommandInfo{Name: "orb", Kind: "skill", Summary: "skill: mine"}, func(string) (string, error) { return "", nil })
		}
		ctx := kernel.NewContext()
		registerOrbCommand(ctx, reg, nil, t.TempDir())
		ctx.Unmount()
		var got *commands.CommandInfo
		for _, c := range reg.List() {
			if c.Name == "orb" {
				got = &c
			}
		}
		switch {
		case !had && got != nil:
			t.Errorf("no skill before, /orb after unmount: %+v", *got)
		case had && (got == nil || got.Summary != "skill: mine"):
			t.Errorf("skill not restored as it was: %+v", got)
		}
	}
}

type stubOrb struct {
	orbLike
	fresh []bool
}

func (s *stubOrb) Restart(fresh bool, by string) string {
	s.fresh = append(s.fresh, fresh)
	return "Restart of orb x scheduled (" + by + ")."
}

// /orb restart forwards to the handle with fresh set only by `fresh`,
// and the usage the palette shows names it.
func TestOrbRestartCommand(t *testing.T) {
	t.Parallel()
	o := &stubOrb{}
	run := orbCommand(o, t.TempDir(), func() []tools.Running { return nil })
	if out, err := run("restart"); err != nil || !strings.Contains(out, "scheduled (person)") {
		t.Fatalf("restart = %q, %v", out, err)
	}
	if _, err := run("restart fresh"); err != nil {
		t.Fatal(err)
	}
	if _, err := run("restart now"); err == nil || !strings.Contains(err.Error(), "usage: /orb restart [fresh]") {
		t.Fatalf("restart now = %v", err)
	}
	if len(o.fresh) != 2 || o.fresh[0] || !o.fresh[1] {
		t.Fatalf("fresh = %v", o.fresh)
	}
	reg := commands.NewRegistry()
	registerOrbCommand(kernel.NewContext(), reg, o, t.TempDir())
	for _, c := range reg.List() {
		if c.Name == "orb" && (!strings.Contains(c.Usage, "restart [fresh]") || !strings.Contains(c.Summary, "restart")) {
			t.Fatalf("/orb info = %+v", c)
		}
	}
}
