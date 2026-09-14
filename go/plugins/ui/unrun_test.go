package ui

import (
	"strings"
	"testing"
)

func TestStripUnrunPrograms(t *testing.T) {
	t.Parallel()
	in := "```js\nconsole.log(tools.bash(\"pwd\"));\n```\n\n```js\nconst x = 1;\n```\n\nBlocked: gh is missing."
	got := strings.TrimSpace(stripUnrunPrograms(in))
	want := "```js\nconst x = 1;\n```\n\nBlocked: gh is missing."
	if got != want {
		t.Fatalf("got %q, want %q", got, want)
	}
}
