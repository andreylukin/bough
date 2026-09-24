package ask

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
)

// A file named for the open ask in the expire dir times it out now, the
// same way the timeout would: the call errors, nothing is answered, and
// the file is consumed. The ask/answer model test needs the timeout at a
// step it picks, and the real one is minutes.
func TestExpireDirTimesOutNow(t *testing.T) {
	t.Parallel()
	fn, a, _, hist, _ := mount(t, nil)
	dir := t.TempDir()
	a.expireDir = dir
	errc := make(chan error, 1)
	go func() { _, err := fn("still there?"); errc <- err }()
	deadline := time.Now().Add(5 * time.Second)
	for {
		a.mu.Lock()
		_, open := a.pending["ask-1"]
		a.mu.Unlock()
		if open {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("ask-1 never opened")
		}
		time.Sleep(5 * time.Millisecond)
	}
	select {
	case err := <-errc:
		t.Fatalf("returned before the expire file: %v", err)
	case <-time.After(100 * time.Millisecond):
	}
	file := filepath.Join(dir, "ask-1")
	if err := os.WriteFile(file, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-errc:
		if err == nil || !strings.Contains(err.Error(), "no answer after") {
			t.Fatalf("err = %v, want the timeout's", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the expire file did not time the ask out")
	}
	if _, err := os.Stat(file); !os.IsNotExist(err) {
		t.Fatalf("expire file left behind: %v", err)
	}
	if err := a.Answer("ask-1", "late"); err == nil {
		t.Fatal("an expired ask still took an answer")
	}
	for _, e := range hist.all() {
		if e.Kind == "ask/answer" {
			t.Fatalf("an expired ask recorded an answer: %v", e.Data)
		}
	}
}

// A "hold" file in the expire dir keeps a native ask from being put in
// until it goes: the ask-beside-parallel-calls model test stands in the
// state where the call has started and the question is not asked yet.
// A cancel while held ends the call as cancelled, with nothing asked.
func TestExpireDirHoldsNativeAsk(t *testing.T) {
	t.Parallel()
	reg, a, _, hist := mountNative(t)
	dir := t.TempDir()
	a.expireDir = dir
	hold := filepath.Join(dir, "hold")
	if err := os.WriteFile(hold, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	asked := make(chan string, 1)
	a.emit = func(ev Event) { asked <- ev.ID }
	tl, _ := reg.Lookup("ask")
	call := func(ctx context.Context) chan agenttools.Result {
		out := make(chan agenttools.Result, 1)
		go func() {
			r, _ := tl.Call(ctx, agenttools.Call{ID: "c1", Args: json.RawMessage(`{"question":"held?"}`)})
			out <- r
		}()
		return out
	}
	ctx, cancel := context.WithCancel(context.Background())
	r := call(ctx)
	time.Sleep(150 * time.Millisecond)
	if es := hist.all(); len(es) != 0 {
		t.Fatalf("asked while held: %+v", es)
	}
	cancel()
	select {
	case res := <-r:
		if !strings.Contains(res.Error, "cancelled") {
			t.Fatalf("cancelled while held = %+v", res)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a cancel did not end a held ask")
	}
	r = call(context.Background())
	time.Sleep(150 * time.Millisecond)
	if es := hist.all(); len(es) != 0 {
		t.Fatalf("asked while held: %+v", es)
	}
	os.Remove(hold)
	select {
	case id := <-asked:
		a.Answer(id, "yes")
	case <-time.After(5 * time.Second):
		t.Fatal("the ask was not put in once the hold went")
	}
	if res := <-r; res.Text != "yes" {
		t.Fatalf("ask after the hold = %+v", res)
	}
}
