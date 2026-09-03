package ui

import (
	"strings"
	"testing"
)

// The exact live sequence seen 2026-09-03: a spawn turn, the parent's
// second reply (prose + a bash fence + guessed prose), then its final
// answer. The parent's second reply must render AFTER the card and
// the spawn result, never above them.
func TestSpawnTurnKeepsEmissionOrder(t *testing.T) {
	d := defaultDrv(t)
	ev := func(kind, text string, data map[string]any) {
		d.feed(eventMsg(Event{Kind: kind, Text: text, Data: data}))
	}
	ev("assistant-delta", "I'll spawn.", nil)
	ev("assistant", "I'll spawn.\n```js\nconsole.log(tools.spawn(\"make notes\"))\n```", nil)
	ev("code", "console.log(tools.spawn(\"make notes\"))\n", nil)
	ev("sub:start", "make notes", map[string]any{"worker": 1})
	ev("sub:assistant", "```js\ntools.write(\"notes.md\",\"x\")\n```", map[string]any{"worker": 1})
	ev("sub:code", "tools.write(\"notes.md\",\"x\")", map[string]any{"worker": 1})
	ev("sub:result", "wrote notes.md", map[string]any{"worker": 1})
	ev("sub:assistant", "Findings: wrote it.", map[string]any{"worker": 1})
	ev("sub:done", "", map[string]any{"worker": 1, "status": "ok", "steps": 2})
	ev("result", "[subagent 1 · task: make notes]\nFindings: wrote it.", nil)
	ev("assistant-delta", "The subagent has finished. Let me verify:", nil)
	ev("assistant", "The subagent has finished. Let me verify:\n```js\nconsole.log(tools.bash(\"cat notes.md\"))\n```\nVerification confirms.", nil)
	ev("code", "console.log(tools.bash(\"cat notes.md\"))\n", nil)
	ev("result", "x", nil)
	ev("assistant", "Done, verified.", nil)
	ev("done", "", map[string]any{"exit": 0})
	var kinds []string
	for _, b := range d.m.blocks {
		kinds = append(kinds, b.kind+":"+strings.SplitN(strings.TrimSpace(b.text+b.label), "\n", 2)[0])
	}
	p := d.plain()
	iCard, iRes, iVerify := strings.Index(p, "subagent 1"), strings.Index(p, "result (2 lines)"), strings.Index(p, "The subagent has finished")
	if !(iCard < iRes && iRes < iVerify) {
		t.Fatalf("parent reply rendered above the card/result (card@%d result@%d reply@%d)\nblocks: %s\n%s", iCard, iRes, iVerify, strings.Join(kinds, " | "), p)
	}
	if strings.Contains(p, "Verification confirms") {
		t.Fatalf("guessed prose should be superseded:\n%s", p)
	}
}
