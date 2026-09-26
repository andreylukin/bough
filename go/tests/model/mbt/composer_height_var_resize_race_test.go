//go:build !windows

package mbt

import (
	"bytes"
	"fmt"
	"image"
	"image/png"
	"io"
	"net/http"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
)

// specs/composer_height_var_resize_race.fizz against a real serve: the
// --composer-h CSS variable a ResizeObserver on the composer writes,
// racing the composer's own layout effect when an attachment is added
// or removed.
//
// attached is real: AddAttachment is the same POST /api/attachments an
// attach makes, so the composer really does grow. tall, cssVar and
// pendingResize are the page's own -- no API reports a CSS variable or
// a queued ResizeObserver callback -- so the adapter plays the page the
// spec's header describes: the callback always measures whatever tall
// is *when it fires*, never a value snapshotted when it was scheduled.
type chAdapter struct {
	t *testing.T
	s *servetest.Server

	id   string
	ids  []string
	gate gate

	attached, tall, cssVar, pendingResize bool

	// staleSnapshot is the deliberate bug
	// TestComposerHeightCatchesWrongAdapter injects: the callback writes
	// the height it measured when it was queued, not when it runs -- the
	// exact race the spec exists to rule out.
	staleSnapshot bool
	scheduledTall bool
}

func newCHAdapter(t *testing.T) *chAdapter {
	s := servetest.Start(t, servetest.Options{})
	return &chAdapter{t: t, s: s}
}

// Init opens a fresh session in the same serve: AddAttachment's upload
// needs somewhere real to land.
func (a *chAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.attached, a.tall, a.cssVar, a.pendingResize = false, false, false, false
	a.scheduledTall = false
	a.gate.reset()
	return nil
}

func (a *chAdapter) Cleanup() error { return nil }

func (a *chAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Composer", Index: 0}: a}, nil
}

func (a *chAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"attached":      a.attached,
		"tall":          a.tall,
		"cssVar":        a.cssVar,
		"pendingResize": a.pendingResize,
	}, nil
}

var chPNG = func() []byte {
	var b bytes.Buffer
	png.Encode(&b, image.NewGray(image.Rect(0, 0, 1, 1)))
	return b.Bytes()
}()

// upload is the real POST an attach makes, growing the composer for
// real: the trigger AddAttachment stands in for.
func (a *chAdapter) upload() error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/attachments", bytes.NewReader(chPNG))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "image/png")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: string(raw)}
	}
	return nil
}

// schedule queues (or coalesces into) the observer callback: a resize
// already in flight stays queued, it does not get a second one.
func (a *chAdapter) schedule() {
	if !a.pendingResize {
		a.scheduledTall = a.tall // only matters to the buggy variant below
	}
	a.pendingResize = true
}

func (a *chAdapter) AddAttachment() error {
	if !a.gate.pass(!a.attached) {
		return nil
	}
	if err := a.upload(); err != nil {
		return fmt.Errorf("AddAttachment: %w", err)
	}
	a.attached, a.tall = true, true
	a.schedule()
	return nil
}

// RemoveAttachment is the composer shrinking back: nothing an API
// reports, so it is applied the way the page's own layout effect does.
func (a *chAdapter) RemoveAttachment() error {
	if !a.gate.pass(a.attached) {
		return nil
	}
	a.attached, a.tall = false, false
	a.schedule()
	return nil
}

// FireObserver is the queued ResizeObserver callback running: it reads
// the composer's height live. staleSnapshot instead writes whatever
// height was current when the callback was queued, which is wrong the
// moment a second attachment change lands before the browser fires it.
func (a *chAdapter) FireObserver() error {
	if !a.gate.pass(a.pendingResize) {
		return nil
	}
	if a.staleSnapshot {
		a.cssVar = a.scheduledTall
	} else {
		a.cssVar = a.tall
	}
	a.pendingResize = false
	return nil
}

var chActions = map[string]map[string]fmbt.ActionFunc{"Composer": {
	"AddAttachment":    action((*chAdapter).AddAttachment),
	"RemoveAttachment": action((*chAdapter).RemoveAttachment),
	"FireObserver":     action((*chAdapter).FireObserver),
}}

func chOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

func TestComposerHeightVarResizeRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCHAdapter(t)
	if err := runMBT(t, "composer_height_var_resize_race", a, chActions, chOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a callback that snapshots the
// height at schedule time -- instead of measuring it live when it fires
// -- fails it: that stale read is exactly the race the spec forbids
// (add then remove an attachment before the coalesced callback runs).
func TestComposerHeightVarResizeRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCHAdapter(t)
	a.staleSnapshot = true
	if err := runMBT(t, "composer_height_var_resize_race", a, chActions, chOptions()); err == nil {
		t.Fatal("a callback that snapshots the height at schedule time passed; the runner is not checking state")
	}
}
