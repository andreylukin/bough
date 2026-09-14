package rules

import "testing"

type legacyHooks struct {
	event, name string
	fn          func(map[string]any) map[string]any
	removed     bool
}

func (h *legacyHooks) Add(event, name string, fn func(map[string]any) map[string]any) func() {
	h.event, h.name, h.fn = event, name, fn
	return func() { h.removed = true }
}

type describedHooks struct {
	legacyHooks
	description string
}

func (h *describedHooks) AddWithDescription(event, name, description string, fn func(map[string]any) map[string]any) func() {
	h.description = description
	return h.Add(event, name, fn)
}

func TestHookDescriptionRegistrationFallback(t *testing.T) {
	t.Parallel()
	for _, extended := range []bool{false, true} {
		t.Run(map[bool]string{false: "legacy", true: "extended"}[extended], func(t *testing.T) {
			t.Parallel()
			legacy := &legacyHooks{}
			var h hooker = legacy
			described := &describedHooks{}
			if extended {
				h, legacy = described, &described.legacyHooks
			}
			remove := addHook(h, "post-result", "rules", "purpose", func(map[string]any) map[string]any {
				return map[string]any{"result": "applied"}
			})
			if legacy.event != "post-result" || legacy.name != "rules" || legacy.fn(nil)["result"] != "applied" {
				t.Fatalf("registration = %#v", legacy)
			}
			if extended && described.description != "purpose" {
				t.Fatalf("description = %q", described.description)
			}
			remove()
			if !legacy.removed {
				t.Fatal("cleanup not forwarded")
			}
		})
	}
}
