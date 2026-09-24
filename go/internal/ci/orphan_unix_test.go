//go:build !windows

package ci

import "testing"

// The agent's bash tool times out by SIGKILLing its own process group:
// bough ci dies, and the check, in a group of its own, lives on. The
// next bough ci must wait for it rather than move the worktree under a
// build that is still writing there.
func TestOrphanedCheckHoldsTheLock(t *testing.T) {
	t.Parallel()
	f := newRepo(t, `checks:
  slow:
    run: "echo x >> $C/start; if [ -e $C/busy ]; then echo x >> $C/overlap; fi; touch $C/busy; sleep 1.5; rm -f $C/busy; echo x >> $C/done"
`)
	h := f.helper()
	if err := h.Start(); err != nil {
		t.Fatal(err)
	}
	if !waitFor(f.exists("start")) {
		h.Process.Kill()
		h.Wait()
		t.Fatal("the helper's check never started")
	}
	h.Process.Kill()
	h.Wait()
	rep := f.run(f.opts())
	if st := state(rep, "slow"); st.State != StatePass || st.Cached {
		t.Fatalf("after the orphan: %+v", st)
	}
	if f.count("overlap") != 0 || f.count("done") != 2 || f.count("start") != 2 {
		t.Fatalf("ran alongside the orphan: overlap %d done %d start %d", f.count("overlap"), f.count("done"), f.count("start"))
	}
}
