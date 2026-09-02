package ui

// Golden frames for the few most stable renders. Regenerate with:
//
//	go test ./plugins/ui -run Golden -update
//
// Everything volatile (markdown, spinner, timestamps) stays out of
// goldens; these four frames are pure deterministic lipgloss output.

import (
	"testing"

	"github.com/charmbracelet/x/exp/golden"
)

func TestGoldenEmptyFrame(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	golden.RequireEqual(t, []byte(d.view()))
}

func TestGoldenUserAndDoneFrame(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hello bough")
	d.event("done", "")
	golden.RequireEqual(t, []byte(d.view()))
}

func TestGoldenCollapsedResultFrame(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	golden.RequireEqual(t, []byte(d.view()))
}

func TestGoldenErrorFrame(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", "llm: no provider mounted")
	golden.RequireEqual(t, []byte(d.view()))
}
