//go:build !windows

package llm

import (
	"fmt"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

// Every session on a serve is its own headless process with its own
// controlLLM over one shared dir, so the mutex does not order them: two
// that list the dir at once both pick the same first name, and the one
// whose rename lost used to fail its turn with ENOENT. A web spec that
// started three sessions side by side saw one of them end in "error".
func TestControlTakeAcrossProcesses(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	const turns = 64
	for i := range turns {
		if err := os.WriteFile(filepath.Join(dir, fmt.Sprintf("t%03d.json", i)), []byte(`{"mode":"ok"}`), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	var (
		wg    sync.WaitGroup
		mu    sync.Mutex
		taken = map[string]int{}
	)
	for range 16 {
		c := &controlLLM{dir: dir} // one per process
		wg.Go(func() {
			for {
				name, _, ok, err := c.take(ullm.Request{}, false, nil)
				if err != nil {
					t.Errorf("take: %v", err)
					return
				}
				if !ok {
					return
				}
				mu.Lock()
				taken[name]++
				mu.Unlock()
			}
		})
	}
	wg.Wait()
	if len(taken) != turns {
		t.Errorf("took %d distinct turns, want %d", len(taken), turns)
	}
	for n, k := range taken {
		if k != 1 {
			t.Errorf("turn %s taken %d times", n, k)
		}
	}
}
