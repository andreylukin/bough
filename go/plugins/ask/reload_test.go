package ask

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// histWithEntries is fakeHist that also serves Entries, like the
// history row's Store: what a resumed session's transcript already has.
type histWithEntries struct{ fakeHist }

func (h *histWithEntries) Entries() []history.Entry { return h.all() }

// mountRow mounts the ask plugin as a kernel row, so a config change
// reconciles it: dispose, then a fresh Asker.
func mountRow(t *testing.T, hist appender) *kernel.Context {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", &fakeCode{})
	ctx.Provide("history", hist)
	if err := ctx.Mount(askRows(10)); err != nil {
		t.Fatalf("mount: %v", err)
	}
	return ctx
}

func askRows(timeout int) []kernel.Row {
	return []kernel.Row{{ID: "ask", Plugin: "ask", Config: map[string]any{"timeout_minutes": timeout}}}
}

func asker(t *testing.T, ctx *kernel.Context) *Asker {
	t.Helper()
	a, err := kernel.Get[*Asker](ctx, "ask-answers")
	if err != nil {
		t.Fatalf("ask-answers: %v", err)
	}
	return a
}

func waitPending(t *testing.T, a *Asker) string {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		a.mu.Lock()
		for id := range a.pending {
			a.mu.Unlock()
			return id
		}
		a.mu.Unlock()
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatal("no ask opened")
	return ""
}

// Disposing the row's Asker (a config reload, a /model swap) ends the
// asks it holds: the new Asker cannot answer them, so the blocked call
// used to wait out the ten-minute timeout while every answer was
// refused with "no pending ask".
// Found by tests/model/mbt/ask_across_reload_respawn_test.go (ReloadAsk).
func TestDisposedAskerCancelsItsAsks(t *testing.T) {
	t.Parallel()
	ctx := mountRow(t, &fakeHist{})
	a := asker(t, ctx)
	errc := make(chan error, 1)
	go func() {
		_, err := a.ask("which?")
		errc <- err
	}()
	waitPending(t, a)
	if err := ctx.Reconcile(askRows(11)); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-errc:
		if err == nil {
			t.Fatal("the disposed Asker's ask returned no error")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the disposed Asker's ask is still blocked")
	}
}

// Ask ids are unique in the session's history: a new Asker (a row
// remount, or a respawned process on the same transcript) continues
// after the asks history already has. Every Asker restarting at ask-1
// let a draft kept for the old ask-1 answer the new one.
// Found by tests/model/mbt/ask_across_reload_respawn_test.go (Respawn,
// ReloadAsk).
func TestAskIDsContinueAcrossAskers(t *testing.T) {
	t.Parallel()
	hist := &histWithEntries{}
	ctx := mountRow(t, hist)
	next := func(a *Asker) string {
		go a.ask("which?")
		id := waitPending(t, a)
		if err := a.Answer(id, "x"); err != nil {
			t.Fatal(err)
		}
		return id
	}
	if id := next(asker(t, ctx)); id != "ask-1" {
		t.Fatalf("first ask id = %q, want ask-1", id)
	}
	if err := ctx.Reconcile(askRows(11)); err != nil {
		t.Fatal(err)
	}
	if id := next(asker(t, ctx)); id != "ask-2" {
		t.Fatalf("after a remount the ask id = %q, want ask-2", id)
	}
	// A respawned process: a fresh kernel on the same transcript.
	if id := next(asker(t, mountRow(t, hist))); id != "ask-3" {
		t.Fatalf("in a respawned process the ask id = %q, want ask-3", id)
	}
}
