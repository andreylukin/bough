//go:build !windows

package session

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"
	"testing/synctest"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/fake"
)

// A call can finish while a different result's request is still in flight.
// Its follow-up must observe the failed response's parking decision even
// when the actor is slower than the coordinator that starts that follow-up.
func TestProviderFailureParksBeforePendingCallRequest(t *testing.T) {
	t.Parallel()
	for _, clm := range []bool{false, true} {
		for _, overflow := range []bool{false, true} {
			t.Run(fmt.Sprintf("clm=%v/overflow=%v", clm, overflow), func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) {
					answer := make(chan struct{})
					errorEntered, releaseError := make(chan struct{}), make(chan struct{})
					var release sync.Once
					defer release.Do(func() { close(releaseError) })
					failure := errBoom
					if overflow {
						failure = fmt.Errorf("prompt too long: %w", agentllm.ErrContextOverflow)
					}
					r := newRigWith(t, []rigOpt{func(d *Deps) {
						d.Config.CLM = clm
						emit := d.Emit
						first := true
						d.Emit = func(kind, text string, data map[string]any) {
							emit(kind, text, data)
							if kind == "error" && first {
								first = false
								close(errorEntered)
								<-releaseError
							}
						}
					}},
						fake.Step{Output: []ullmItem{fake.Call("h1", "hold", `{"text":"work"}`), fake.Call("q1", "fail", `{}`)}},
						fake.Step{Hold: answer, Err: failure},
						fake.Step{Output: []ullmItem{fake.Text("recovered")}},
					)
					r.rt.Submit("work")
					r.waitRequests(2)
					close(r.kit.release)
					r.waitFor("both call results", func() bool { return r.count("call") == 2 })
					close(answer)
					<-errorEntered
					// The actor is held before Park. The coordinator is free to
					// persist the next request and run its Gate up to a barrier.
					r.waitFor("the completed call's follow-up request", func() bool {
						for _, reason := range r.rt.sync.turnReasons() {
							if reason == "call:h1" {
								return true
							}
						}
						return false
					})
					synctest.Wait()
					release.Do(func() { close(releaseError) })
					r.waitDone(1)
					synctest.Wait()
					if r.count("input") != 1 || r.count("error") != 1 || r.count("done") != 1 || len(r.fake.Requests()) != 2 {
						t.Fatalf("the completed call retried the failed turn\n%s", r.dump())
					}
					r.rt.Submit("retry")
					r.waitDone(2)
					if overflow {
						if r.count("error") != 2 || len(r.fake.Requests()) != 2 {
							t.Fatalf("the new input forgot the sticky overflow\n%s", r.dump())
						}
					} else if r.count("error") != 1 || len(r.fake.Requests()) != 3 || r.last("assistant").Data["text"] != "recovered" {
						t.Fatalf("the new input did not unpark the provider\n%s", r.dump())
					}
				})
			})
		}
	}
}

// A stopped or superseded request must not wait for a busy actor to
// finish its hook or shutdown before the coordinator can let it go.
func TestGateActorBarrierCanBeCancelled(t *testing.T) {
	t.Parallel()
	for _, shutdown := range []bool{false, true} {
		t.Run(fmt.Sprintf("shutdown=%v", shutdown), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				r := &Runtime{q: newFIFO(), exited: make(chan struct{}), sync: &syncMirror{m: newMirror()}}
				g := newGate(r, "", nil)
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				done := make(chan error, 1)
				go func() {
					_, err := g.Respond(ctx, ullm.Request{}, ullm.RequestOptions{})
					done <- err
				}()
				synctest.Wait()
				if shutdown {
					close(r.exited)
				} else {
					cancel()
				}
				if err := <-done; !errors.Is(err, context.Canceled) {
					t.Fatalf("barrier returned %v, want cancellation", err)
				}
				if g.seq != 0 {
					t.Fatal("the cancelled request reached the gate")
				}
			})
		})
	}
}
