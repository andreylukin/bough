package loop

import (
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// notifyNotices is a job-notices seam with Notify, like tools' Jobs.
type notifyNotices struct {
	mu   sync.Mutex
	news []string
	all  []string
	wake chan struct{}
}

func newNotifyNotices() *notifyNotices { return &notifyNotices{wake: make(chan struct{}, 1)} }

func (n *notifyNotices) Take() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	s := n.news
	n.news = nil
	return s
}
func (n *notifyNotices) Wake() <-chan struct{} { return n.wake }
func (n *notifyNotices) Notify(text string) {
	n.mu.Lock()
	n.news = append(n.news, text)
	n.all = append(n.all, text)
	n.mu.Unlock()
	select {
	case n.wake <- struct{}{}:
	default:
	}
}
func (n *notifyNotices) delivered() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	return append([]string(nil), n.all...)
}

// seedNotices writes a session with two undelivered notices for it,
// one already delivered, and one addressed to another session.
func seedNotices(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "parent.jsonl")
	s, err := history.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("meta", map[string]any{"cwd": "/"})
	s.Close()
	for _, d := range []map[string]any{
		{"id": "n1", "to": "parent", "text": "first"},
		{"id": "n0", "to": "parent", "text": "old"},
		{"id": "nx", "to": "someone-else", "text": "not mine"},
		{"id": "n2", "to": "parent", "text": "second"},
	} {
		if _, err := history.AppendFile(path, "notice", d); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := history.AppendFile(path, "notice-delivered", map[string]any{"id": "n0"}); err != nil {
		t.Fatal(err)
	}
	return path
}

func mountStored(t *testing.T, path string, no *notifyNotices) (func() []string, chan struct{}, func()) {
	t.Helper()
	store, err := history.OpenExisting(path)
	if err != nil {
		t.Fatal(err)
	}
	llm := &steerDeepLLM{replies: []string{"ok"}}
	kctx := kernel.NewContext()
	kctx.Provide("llm", LLM(llm))
	kctx.Provide("codemode", Codemode(&stubCode{}))
	kctx.Provide("history", History(store))
	if no != nil {
		kctx.Provide("job-notices", Notices(no))
	}
	var mu sync.Mutex
	var ks []string
	done := make(chan struct{}, 4)
	kctx.On("loop/event", func(p any) {
		ev := p.(Event)
		mu.Lock()
		ks = append(ks, ev.Kind)
		mu.Unlock()
		if ev.Kind == "done" {
			done <- struct{}{}
		}
	})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	stop := func() { kctx.Unmount(); store.Close() }
	return func() []string { mu.Lock(); defer mu.Unlock(); return append([]string(nil), ks...) }, done, stop
}

func countKind(t *testing.T, path, kind string) int {
	t.Helper()
	es, err := history.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for _, e := range es {
		if e.Kind == kind {
			n++
		}
	}
	return n
}

// A stopped session's stored notices wake it once on mount, in seq
// order, each marked delivered; a second mount delivers nothing.
func TestStoredNoticesDeliveredOnceOnMount(t *testing.T) {
	t.Parallel()
	path := seedNotices(t)
	no := newNotifyNotices()
	kinds, done, stop := mountStored(t, path, no)
	waitDone(t, done, kinds)
	stop()
	if got := strings.Join(no.delivered(), "|"); got != "first|second" {
		t.Fatalf("delivered %q, want first|second", got)
	}
	if n := countKind(t, path, "notice-delivered"); n != 3 {
		t.Fatalf("notice-delivered entries = %d, want 3", n)
	}

	again := newNotifyNotices()
	_, _, stop2 := mountStored(t, path, again)
	time.Sleep(100 * time.Millisecond)
	stop2()
	if got := again.delivered(); len(got) != 0 {
		t.Fatalf("remount re-delivered %q", got)
	}
}

// A fork's file carries the parent's notices, addressed to the parent:
// the fork delivers none.
func TestStoredNoticesNotDeliveredInFork(t *testing.T) {
	t.Parallel()
	path := seedNotices(t)
	// Fork needs a finished turn past the notices.
	in, err := history.AppendFile(path, "input", map[string]any{"text": "hi"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := history.AppendFile(path, "done", nil); err != nil {
		t.Fatal(err)
	}
	fork := filepath.Join(filepath.Dir(path), "fork.jsonl")
	if err := history.Fork(path, in.Seq, fork); err != nil {
		t.Fatal(err)
	}
	no := newNotifyNotices()
	_, _, stop := mountStored(t, fork, no)
	time.Sleep(100 * time.Millisecond)
	stop()
	if got := no.delivered(); len(got) != 0 {
		t.Fatalf("fork delivered %q", got)
	}
}

// Without Notify on the seam nothing is marked, so nothing is lost.
func TestStoredNoticesNoNotifySeam(t *testing.T) {
	t.Parallel()
	path := seedNotices(t)
	no := &steerDeepNotices{wake: make(chan struct{}, 1)}
	store, err := history.OpenExisting(path)
	if err != nil {
		t.Fatal(err)
	}
	kctx := kernel.NewContext()
	kctx.Provide("llm", LLM(&steerDeepLLM{replies: []string{"ok"}}))
	kctx.Provide("codemode", Codemode(&stubCode{}))
	kctx.Provide("history", History(store))
	kctx.Provide("job-notices", Notices(no))
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatal(err)
	}
	time.Sleep(100 * time.Millisecond)
	kctx.Unmount()
	store.Close()
	if n := countKind(t, path, "notice-delivered"); n != 1 {
		t.Fatalf("notice-delivered entries = %d, want the seeded 1", n)
	}
}

// A notice appended while the session is already running (a TUI
// parent serve does not run) reaches it without a remount, once.
func TestStoredNoticeDeliveredWhileRunning(t *testing.T) {
	t.Parallel()
	path := seedNotices(t)
	no := newNotifyNotices()
	kinds, done, stop := mountStored(t, path, no)
	defer stop()
	waitDone(t, done, kinds)
	if _, err := history.AppendFile(path, "notice", map[string]any{"id": "n3", "to": "parent", "text": "late"}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for !strings.HasSuffix(strings.Join(no.delivered(), "|"), "|late") {
		if time.Now().After(deadline) {
			t.Fatalf("delivered %q, want the late notice", no.delivered())
		}
		time.Sleep(20 * time.Millisecond)
	}
	waitDone(t, done, kinds)
	time.Sleep(1500 * time.Millisecond) // another poll: must not re-deliver
	if got := strings.Join(no.delivered(), "|"); got != "first|second|late" {
		t.Fatalf("delivered %q", got)
	}
}
