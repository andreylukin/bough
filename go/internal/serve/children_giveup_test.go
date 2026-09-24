package serve

import (
	"context"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

// A spawn the client gave up on (its serveTimeout fired, or Esc cancelled
// the turn) was told to the model as failed: serve must not leave an
// agent running under it, whether it saw the give-up before it made the
// child or while its answer was still on the way. Kept, the child ran
// on, a retry made a second one, and its "[agent … finished]" named an
// id the parent never had.
func TestChildCreateGivenUpLeavesNoChild(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	f.seed(t, "parent")
	holds := t.TempDir()
	f.api.getenv = func(k string) string {
		if k == "BOUGH_TEST_CREATE_HOLD_DIR" {
			return holds
		}
		return ""
	}
	create := func(ctx context.Context) {
		f.api.createChild(ctx, httptest.NewRecorder(), CreateOptions{Prompt: "HANG", SpawnedBy: "parent"}, 0, 0)
	}

	// Given up before serve made it.
	gone, cancel := context.WithCancel(context.Background())
	cancel()
	create(gone)
	if kids := f.sup.Children("parent"); len(kids) != 0 {
		t.Fatalf("a create given up before serve made the child left %d", len(kids))
	}

	// Given up after serve made it, before the answer went out.
	if err := os.WriteFile(filepath.Join(holds, "respond.hold"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	returned := make(chan struct{})
	go func() { create(ctx); close(returned) }()
	var id string
	waitFor(t, "serve to make the child", func() bool {
		b, _ := os.ReadFile(filepath.Join(holds, "respond.held"))
		id = string(b)
		return id != ""
	})
	waitFor(t, "the child to run", func() bool { return f.sup.Live(id) })
	cancel()
	<-returned
	waitFor(t, "the child to be withdrawn", func() bool { return !f.sup.Live(id) })
	if kids := f.sup.Children("parent"); len(kids) != 0 {
		t.Fatalf("a create given up after serve made the child left %v", kids)
	}
	if n := notices(t, f.fixture, "parent"); len(n) != 0 {
		t.Fatalf("the parent was told about an agent it never had: %v", n)
	}
}
