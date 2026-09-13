package activity

import (
	"context"
	"sync"
	"testing"

	"github.com/andreylukin/bough/plugins/llm"
)

type slowLLM struct{ gate map[string]chan struct{} }

func (s slowLLM) Complete(ctx context.Context, _ string, msgs []llm.Message) (string, error) {
	<-s.gate[msgs[0].Content]
	return "label for " + msgs[0].Content, nil
}

// A slow label for an earlier program must not land over a newer one.
func TestStaleLabelDropped(t *testing.T) {
	gate := map[string]chan struct{}{"one": make(chan struct{}), "two": make(chan struct{})}
	var mu sync.Mutex
	var got []string
	a := &Activity{llm: slowLLM{gate}, ctx: context.Background(), emit: func(s string) { mu.Lock(); got = append(got, s); mu.Unlock() }}
	var wg sync.WaitGroup
	wg.Add(1)
	go func() { defer wg.Done(); a.label("one") }()
	for {
		mu.Lock()
		n := len(got)
		mu.Unlock()
		if n == 1 {
			break
		}
	}
	wg.Add(1)
	go func() { defer wg.Done(); a.label("two") }()
	for {
		mu.Lock()
		n := len(got)
		mu.Unlock()
		if n == 2 {
			break
		}
	}
	close(gate["two"])
	close(gate["one"])
	wg.Wait()
	want := []string{"", "", "label for two"}
	if len(got) != len(want) {
		t.Fatalf("got %q, want %q", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %q, want %q", got, want)
		}
	}
}
