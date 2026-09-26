//go:build !windows

package mbt

import (
	"fmt"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"
)

// specs/code_block_lazy_highlight_scroll.fizz: a transcript code block
// (go/internal/serve/web/src/code.tsx) mounting, scrolling into view
// (the only point it calls highlight()) and scrolling back out of a
// virtualized transcript, for two blocks that can be at any of those
// points independently and concurrently.
//
// Nothing here is a server API: BY_EXT/hljs.getLanguage, mounting and
// scrolling are all the page's own, so the adapter plays the page
// exactly the way the spec's header describes, as composer_height_var
// _resize_race_test.go's chAdapter does for its ResizeObserver. The
// browser spec (specs/model/code_block_lazy_highlight_scroll.spec.ts)
// is what checks this against the real component.

// codeBlockAdapter is the fmbt.Model; its two roles are the two
// CodeBlock instances the spec walks concurrently.
type codeBlockAdapter struct {
	t    *testing.T
	gate gate

	a, b *codeBlock

	// wrongFallback is the deliberate wiring bug
	// TestCodeBlockLazyHighlightScrollCatchesWrongAdapter injects: an
	// unknown language is left "none" once visible instead of falling
	// back to "plain", the kind of dropped case a real adapter (or a
	// real langForPath/Code component) could have.
	wrongFallback bool
}

// codeBlock is one CodeBlock role: mounted/visible/lang/rendered mirror
// the spec's fields exactly, by bare name.
type codeBlock struct {
	a *codeBlockAdapter

	mounted, visible bool
	lang, rendered   string
}

func newCodeBlockAdapter(t *testing.T) *codeBlockAdapter {
	a := &codeBlockAdapter{t: t}
	a.a = &codeBlock{a: a}
	a.b = &codeBlock{a: a}
	return a
}

// Init resets both blocks to unmounted, matching the spec's Init.
func (a *codeBlockAdapter) Init() error {
	for _, c := range []*codeBlock{a.a, a.b} {
		c.mounted, c.visible, c.lang, c.rendered = false, false, "known", "none"
	}
	a.gate.reset()
	return nil
}

func (a *codeBlockAdapter) Cleanup() error { return nil }

func (a *codeBlockAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{
		{RoleName: "CodeBlock", Index: 0}: a.a,
		{RoleName: "CodeBlock", Index: 1}: a.b,
	}, nil
}

// GetState is the spec's global state, which holds only the roles.
func (a *codeBlockAdapter) GetState() (map[string]any, error) { return map[string]any{}, nil }

func (c *codeBlock) GetState() (map[string]any, error) {
	return map[string]any{
		"mounted":  c.mounted,
		"visible":  c.visible,
		"lang":     c.lang,
		"rendered": c.rendered,
	}, nil
}

func (c *codeBlock) MountKnown() error {
	if !c.a.gate.pass(!c.mounted) {
		return nil
	}
	c.mounted, c.visible, c.lang, c.rendered = true, false, "known", "none"
	return nil
}

func (c *codeBlock) MountUnknown() error {
	if !c.a.gate.pass(!c.mounted) {
		return nil
	}
	c.mounted, c.visible, c.lang, c.rendered = true, false, "unknown", "none"
	return nil
}

// ScrollIntoView is the only point highlight() actually runs: a known
// language renders highlighted, anything else falls back to plain.
func (c *codeBlock) ScrollIntoView() error {
	if !c.a.gate.pass(c.mounted && !c.visible) {
		return nil
	}
	c.visible = true
	if c.lang == "known" {
		c.rendered = "highlighted"
	} else if c.a.wrongFallback {
		c.rendered = "none"
	} else {
		c.rendered = "plain"
	}
	return nil
}

// ScrollAway unmounts the block exactly like a fresh one.
func (c *codeBlock) ScrollAway() error {
	if !c.a.gate.pass(c.mounted && c.visible) {
		return nil
	}
	c.mounted, c.visible, c.rendered = false, false, "none"
	return nil
}

var codeBlockActions = map[string]map[string]fmbt.ActionFunc{"CodeBlock": {
	"MountKnown":     action((*codeBlock).MountKnown),
	"MountUnknown":   action((*codeBlock).MountUnknown),
	"ScrollIntoView": action((*codeBlock).ScrollIntoView),
	"ScrollAway":     action((*codeBlock).ScrollAway),
}}

// Every action is in-process, so a short run reaches every state and
// transition of both blocks many times over.
func codeBlockOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

func TestCodeBlockLazyHighlightScroll(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCodeBlockAdapter(t)
	if err := runMBT(t, "code_block_lazy_highlight_scroll", a, codeBlockActions, codeBlockOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a block that drops the unknown
// fallback (left "none" once visible, instead of "plain") fails it.
func TestCodeBlockLazyHighlightScrollCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCodeBlockAdapter(t)
	a.wrongFallback = true
	if err := runMBT(t, "code_block_lazy_highlight_scroll", a, codeBlockActions, codeBlockOptions()); err == nil {
		t.Fatal(fmt.Sprintf("a block whose ScrollIntoView leaves an unknown language %q passed; the runner is not checking state", "none"))
	}
}
