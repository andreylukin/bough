package orb

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
	"testing/synctest"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
)

func TestOssaRuntimeReleaseBeforeCall(t *testing.T) {
	t.Parallel()
	for _, which := range []string{"build", "start"} {
		t.Run(which, func(t *testing.T) {
			t.Parallel()
			rt := &ossaRuntime{Fake: container.NewFake()}
			rt.armBuild("fixture")
			rt.armStart("fixture")
			want := errors.New("walk ended before the runtime call")
			if !rt.release(which, want) {
				t.Fatal("cleanup missed an armed hold that the runtime has not reached")
			}
			if rt.release(which, errors.New("second cleanup")) {
				t.Fatal("repeated cleanup replaced the queued reply")
			}
			var err error
			if which == "build" {
				err = rt.Commit(context.Background(), container.CommitSpec{Tag: "bough-orb/fixture:next"}, io.Discard)
			} else {
				err = rt.Start(context.Background(), container.RunSpec{Name: "fixture"})
			}
			if !errors.Is(err, want) {
				t.Fatalf("runtime call = %v, want the queued cleanup error", err)
			}
			if got := rt.held(); got != "" {
				t.Fatalf("runtime still held at %q after cleanup", got)
			}
			if rt.release(which, nil) {
				t.Fatal("completed hold still accepted a reply")
			}
		})
	}
}

func TestOssaRuntimeCancelHeldCall(t *testing.T) {
	t.Parallel()
	for _, which := range []string{"build", "start"} {
		t.Run(which, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				rt := &ossaRuntime{Fake: container.NewFake()}
				rt.armBuild("fixture")
				rt.armStart("fixture")
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				done := make(chan error, 1)
				go func() {
					if which == "build" {
						done <- rt.Commit(ctx, container.CommitSpec{Tag: "bough-orb/fixture:next"}, io.Discard)
					} else {
						done <- rt.Start(ctx, container.RunSpec{Name: "fixture"})
					}
				}()
				synctest.Wait()
				if got := rt.held(); got != which {
					t.Fatalf("held = %q, want %q", got, which)
				}
				cancel()
				synctest.Wait()
				select {
				case err := <-done:
					if !errors.Is(err, context.Canceled) {
						t.Fatalf("runtime call = %v, want context cancellation", err)
					}
				default:
					// Let the broken implementation exit too, so a regression
					// reports its cause instead of hanging the entire package.
					rt.release(which, errors.New("test cleanup"))
					<-done
					t.Fatal("runtime hold ignored context cancellation")
				}
				if got := rt.held(); got != "" {
					t.Fatalf("runtime still held at %q after cancellation", got)
				}
				if rt.release(which, nil) {
					t.Fatal("canceled hold still accepted a reply")
				}
			})
		})
	}
}

func TestOssaBuildDoneWaitsForStartHold(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		a := newOssaAdapter(t)
		a.slug, a.session = "fixture", "fixture"
		a.h = newHandle(a.home, a.session, a.slug, a.rt, "", func() {})
		// Orb.run publishes this phase before Inspect and Start. Hold
		// that ordering open without relying on filesystem/scheduler speed.
		dir := iorb.Dir(a.home, a.session)
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		state, err := json.Marshal(iorb.State{Session: a.session, Status: iorb.StatusStarting, Phase: iorb.PhaseContainer})
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, "state.json"), state, 0o644); err != nil {
			t.Fatal(err)
		}
		a.rt.armBuild(a.slug)
		buildDone := make(chan error, 1)
		go func() {
			buildDone <- a.rt.Commit(context.Background(), container.CommitSpec{Tag: "bough-orb/fixture:next"}, io.Discard)
		}()
		synctest.Wait()
		done := make(chan error, 1)
		go func() { done <- a.BuildDone() }()
		synctest.Wait()
		select {
		case err := <-done:
			t.Fatalf("BuildDone returned before Start reached its hold: %v", err)
		default:
		}
		if err := <-buildDone; err != nil {
			t.Fatal(err)
		}
		startDone := make(chan error, 1)
		go func() {
			startDone <- a.rt.Start(context.Background(), container.RunSpec{Name: container.OrbName(a.session)})
		}()
		if err := <-done; err != nil {
			t.Fatal(err)
		}
		if got := a.rt.held(); got != "start" {
			t.Fatalf("BuildDone returned with held=%q, want start", got)
		}
		want := errors.New("test cleanup")
		a.rt.release("start", want)
		if err := <-startDone; !errors.Is(err, want) {
			t.Fatalf("Start = %v, want cleanup error", err)
		}
	})
}
