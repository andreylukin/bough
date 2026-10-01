//go:build !windows

package session

import (
	"fmt"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"

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
