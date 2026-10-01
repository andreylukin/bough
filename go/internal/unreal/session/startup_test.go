//go:build !windows

package session

import (
	"context"
	"testing"
	"testing/synctest"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/fake"
)

// Headless stdin and stored notices can arrive while the launcher is
// still remounting rows with newly available optional dependencies.
// Freezing that temporary tool set made a resumed parent's spawn calls
// resolve to tombstones even after the workers row had returned.
func TestStartupWaitsForTools(t *testing.T) {
	t.Parallel()
	for _, notice := range []bool{false, true} {
		name := "input"
		if notice {
			name = "notice"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				ready := make(chan struct{})
				jobs := &fakeJobs{wake: make(chan struct{}, 1)}
				r := newRigWith(t, []rigOpt{func(d *Deps) {
					d.Ready = ready
					d.Jobs = func() Jobs { return jobs }
				}},
					fake.Step{Want: "delegate", Output: []ullmItem{fake.Call("s1", "spawn", `{}`)}},
					fake.Step{Want: "child finished", Output: []ullmItem{fake.Text("done")}},
				)
				if notice {
					jobs.notify("delegate")
				} else {
					r.rt.Submit("delegate")
				}
				synctest.Wait()
				if r.count("engine") != 0 || len(r.fake.Requests()) != 0 {
					t.Fatalf("started before the tools were ready:\n%s", r.dump())
				}
				if _, err := r.kit.reg.Register(agenttools.Tool{
					Name: "spawn", Schema: agenttools.Object(nil, map[string]any{}),
					Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
						return agenttools.Result{Text: "child finished"}, nil
					},
				}); err != nil {
					t.Fatal(err)
				}
				close(ready)
				r.waitDone(1)
				if r.count("engine") != 1 || len(r.fake.Requests()) != 2 || r.last("assistant").Data["text"] != "done" {
					t.Fatalf("queued work was lost or duplicated:\n%s", r.dump())
				}
			})
		})
	}
}

func TestCloseBeforeStartupReady(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		r := newRigWith(t, []rigOpt{func(d *Deps) { d.Ready = make(chan struct{}) }})
		r.rt.Submit("do not start")
		synctest.Wait()
		if err := r.rt.Close(context.Background()); err != nil {
			t.Fatal(err)
		}
		if len(r.fake.Requests()) != 0 {
			t.Fatal("called the model before startup finished")
		}
	})
}

func TestCancelBeforeStartupReadyDoesNotExpire(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ready := make(chan struct{})
		r := newRigWith(t, []rigOpt{func(d *Deps) { d.Ready = ready }},
			fake.Step{Want: "cancel this", Hold: make(chan struct{})},
		)
		r.rt.Submit("cancel this")
		r.rt.Cancel()
		synctest.Wait()
		time.Sleep(pendingCancelFor + time.Second)
		close(ready)
		r.waitDone(1)
		if r.count("cancelled") != 1 {
			t.Fatalf("startup delay lost the cancellation:\n%s", r.dump())
		}
	})
}

func TestCancelDuringEmptyStartupDoesNotCancelLaterInput(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ready := make(chan struct{})
		r := newRigWith(t, []rigOpt{func(d *Deps) { d.Ready = ready }},
			fake.Step{Want: "later", Output: []ullmItem{fake.Text("ok")}},
		)
		r.rt.Cancel()
		synctest.Wait()
		close(ready)
		time.Sleep(pendingCancelFor + time.Second)
		r.rt.Submit("later")
		r.waitDone(1)
		if r.count("cancelled") != 0 || r.last("assistant").Data["text"] != "ok" {
			t.Fatalf("an old startup cancellation affected new work:\n%s", r.dump())
		}
	})
}
