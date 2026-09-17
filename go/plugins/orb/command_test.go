package orb

import (
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
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
