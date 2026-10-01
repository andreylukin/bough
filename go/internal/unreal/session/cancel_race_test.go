//go:build !windows

package session

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/fake"
)

// A response makes its calls visible before translation gives them
// operation IDs. Esc in that gap must follow the IDs when they arrive,
// even if the cancel's bounded wait has already closed the turn.
func TestCancelBeforeFirstToolStatus(t *testing.T) {
	t.Parallel()
	for _, clm := range []bool{false, true} {
		for _, late := range []bool{false, true} {
			t.Run(fmt.Sprintf("clm=%v/late=%v", clm, late), func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) {
					r := newRigWith(t, []rigOpt{func(d *Deps) { d.Config.CLM = clm }},
						fake.Step{Want: "slow", Output: []ullmItem{fake.Call("h1", "hold", `{"text":"work"}`)}},
						fake.Step{Want: "next", Match: func(req ullmRequest) error {
							if !strings.Contains(fake.Render(req), "Cancelled") {
								return errf("the cancelled result is not in the request")
							}
							return nil
						}, Output: []ullmItem{fake.Text("moving on")}},
					)
					canceled := make(chan bool, 1)
					release := make(chan struct{})
					var once sync.Once
					t.Cleanup(func() { once.Do(func() { close(release) }) })
					first := true
					r.rt.store.AddObserver(func(_ session.ID, it sessionstore.Item) {
						if it.Kind != sessionstore.ItemModelResponse || !first {
							return
						}
						first = false
						// The runtime observer already queued the response. Hold
						// its writer until Cancel and this actor fence land, so
						// no translator can have published the first status yet.
						r.rt.Cancel()
						r.rt.post(func() {
							c := r.rt.a.m.Outstanding["h1"]
							canceled <- c != nil && len(c.Ops) == 0 && r.rt.a.cancelled["h1"]
						})
						<-release
					})
					r.rt.Submit("slow work")
					if !<-canceled {
						t.Fatal("cancel did not land before the first operation IDs")
					}
					if late {
						r.waitDone(1)
					}
					once.Do(func() { close(release) })
					r.waitDone(1)
					if late {
						r.waitFor("the late cancelled call", func() bool { return r.count("call") == 1 })
					} else if got, want := r.turnKinds(), []string{"input", "call", "cancelled", "done"}; !slices.Equal(got, want) {
						t.Fatalf("kinds %v, want %v\n%s", got, want, r.dump())
					}
					if c := r.last("call"); c.Data["canceled"] != true {
						t.Fatalf("call was not cancelled: %v\n%s", c.Data, r.dump())
					}
					r.stays("no request after the cancel", 1500*time.Millisecond, func() bool { return len(r.fake.Requests()) == 1 })
					r.rt.Submit("next thing")
					r.waitDone(2)
					if r.count("cancelled") != 1 || len(r.fake.Requests()) != 2 || r.last("assistant").Data["text"] != "moving on" {
						t.Fatalf("the next turn did not resume normally\n%s", r.dump())
					}
					fake.AssertAppendOnly(t, r.fake.Requests())
				})
			})
		}
	}
}

// The UI's one-second cancel window can close before a native tool's
// two-second cleanup grace. Process shutdown must still persist its
// cancelled result instead of discarding the late update.
func TestCloseKeepsLateCancelledCall(t *testing.T) {
	t.Parallel()
	for _, clm := range []bool{false, true} {
		for _, limit := range []time.Duration{100 * time.Millisecond, 3 * time.Second} {
			t.Run(fmt.Sprintf("clm=%v/limit=%s", clm, limit), func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) {
					defer func() { time.Sleep(3 * time.Second) }() // drain delayed tool cleanup even on failure
					started := make(chan struct{})
					reg := agenttools.NewRegistry()
					_, err := reg.Register(agenttools.Tool{Name: "slow", Schema: agenttools.Object(nil, nil),
						Call: func(ctx context.Context, _ agenttools.Call) (agenttools.Result, error) {
							close(started)
							<-ctx.Done()
							time.Sleep(1500 * time.Millisecond)
							return agenttools.Result{}, ctx.Err()
						}})
					if err != nil {
						t.Fatal(err)
					}
					r := newRigWith(t, []rigOpt{func(d *Deps) { d.Config.CLM, d.Tools = clm, reg }},
						fake.Step{Want: "slow", Output: []ullmItem{fake.Call("slow1", "slow", `{}`)}},
					)
					r.rt.Submit("slow work")
					<-started
					r.rt.Cancel()
					r.waitDone(1)
					if r.count("call") != 0 {
						t.Fatal("slow cleanup did not outlast the UI cancel window")
					}
					ctx, cancel := context.WithTimeout(context.Background(), limit)
					defer cancel()
					began := time.Now()
					closed := make(chan error, 1)
					go func() { closed <- r.rt.Close(ctx) }()
					r.waitFor("shutdown to start", r.rt.closing.Load)
					r.rt.Submit("do not start another turn")
					// Both the root and child-worker gates must suppress provider
					// work while the coordinator drains already-cancelled results.
					for _, g := range []*Gate{r.rt.gate, newGate(r.rt, "child", nil)} {
						if _, err := g.Respond(context.Background(), ullm.Request{}, ullm.RequestOptions{}); err != nil {
							t.Fatal(err)
						}
					}
					if err := <-closed; err != nil {
						t.Fatal(err)
					}
					if time.Since(began) > limit+10*time.Millisecond {
						t.Fatal("Close exceeded its caller's budget")
					}
					if limit < time.Second {
						if ctx.Err() == nil {
							t.Fatal("Close skipped the pending cancellation instead of waiting to its deadline")
						}
					} else if r.count("call") != 1 || r.last("call").Data["canceled"] != true {
						t.Fatalf("shutdown lost the late canceled call:\n%s", r.dump())
					}
					if r.count("done") != 1 || r.count("cancelled") != 1 || r.count("input") != 1 || len(r.fake.Requests()) != 1 {
						t.Fatalf("shutdown changed the cancelled turn:\n%s", r.dump())
					}
					before := r.dump()
					if err := r.rt.Close(context.Background()); err != nil || r.dump() != before {
						t.Fatal("a repeated Close changed the session")
					}
				})
			})
		}
	}
}

// A coordinator can mark its request in flight before DeltaStart opens
// the actor's wake turn. Close must cancel that request even with no
// open turn, and must not fabricate a turn just to shut it down.
func TestCloseCancelsInflightBeforeWake(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		request, cancelRequest := context.WithCancel(context.Background())
		defer cancelRequest()
		ready := make(chan struct{})
		r.rt.post(func() {
			r.rt.a.m.Inflight = true
			r.rt.gate.mu.Lock()
			r.rt.gate.inflight = cancelRequest
			r.rt.gate.mu.Unlock()
			close(ready)
		})
		<-ready
		go func() {
			<-request.Done()
			r.rt.post(func() { r.rt.a.m.Inflight = false })
		}()
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if err := r.rt.Close(ctx); err != nil {
			t.Fatal(err)
		}
		if request.Err() == nil || ctx.Err() != nil {
			t.Fatal("Close did not cancel the pre-wake request before its deadline")
		}
		if r.count("input") != 0 || r.count("done") != 0 || len(r.fake.Requests()) != 0 {
			t.Fatalf("Close fabricated a wake turn:\n%s", r.dump())
		}
	})
}
