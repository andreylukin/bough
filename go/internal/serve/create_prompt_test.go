package serve

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A pipe write is not a durable acknowledgement: a restart immediately
// after the 201 used to kill the child before it recorded the prompt
// (create_races_failures walk 61, DeliverB -> ServeRestart -> RecordedB).
func TestCreateWaitsForFirstPrompt(t *testing.T) {
	t.Parallel()
	hold := filepath.Join(t.TempDir(), "input")
	if err := os.WriteFile(hold, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	f := newFixture(t, envTurns+"=1", envHoldInput+"="+hold)
	id := history.NewID()
	answer := make(chan error, 1)
	go func() {
		_, err := f.sup.Create(CreateOptions{ID: id, Cwd: f.home, Prompt: "first prompt"})
		answer <- err
	}()
	waitFor(t, "the child's pre-input gate", func() bool {
		_, err := os.Stat(hold + ".at")
		return err == nil
	})
	select {
	case err := <-answer:
		t.Fatalf("Create answered before the held child recorded its prompt: %v", err)
	case <-time.After(200 * time.Millisecond):
	}
	if err := os.Remove(hold); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-answer:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Create never acknowledged the recorded prompt")
	}
	// Nothing waits for an output or another request before restarting.
	f.sup.Close()
	es, err := f.sup.Entries(id)
	if err != nil {
		t.Fatal(err)
	}
	var inputs []string
	for _, e := range es {
		if e.Kind == "input" {
			inputs = append(inputs, inputText(e.Data))
		}
	}
	if len(inputs) != 1 || inputs[0] != "first prompt" {
		t.Fatalf("inputs after immediate restart = %q, want one first prompt", inputs)
	}
}

func TestCreateFailsIfChildExitsBeforeFirstPrompt(t *testing.T) {
	t.Parallel()
	hold := filepath.Join(t.TempDir(), "input")
	if err := os.WriteFile(hold, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	f := newFixture(t, envTurns+"=1", envHoldInput+"="+hold)
	id := history.NewID()
	answer := make(chan error, 1)
	go func() {
		_, err := f.sup.Create(CreateOptions{ID: id, Cwd: f.home, Prompt: "unread"})
		answer <- err
	}()
	waitFor(t, "the first prompt to reach stdin", func() bool {
		f.sup.mu.Lock()
		defer f.sup.mu.Unlock()
		ch := f.sup.kids[id]
		return ch != nil && ch.unread
	})
	if err := f.sup.Kill(id); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-answer:
		if err == nil {
			t.Fatal("Create acknowledged a prompt the child never read")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Create did not notice the child's exit")
	}
	if _, err := os.Stat(filepath.Join(f.hist, id+".jsonl")); !os.IsNotExist(err) {
		t.Fatalf("failed create left its history file: %v", err)
	}
	if f.sup.Live(id) {
		t.Fatal("failed create kept a live child")
	}
}

func TestWaitCreatePromptAcknowledgements(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		prompt string
		kind   string
		seq    int64
		data   map[string]any
		tail   []history.Entry
		exit   bool
		wantOK bool
	}{
		{name: "recorded input", prompt: " hello ", kind: "input", seq: 3, data: map[string]any{"text": "hello"}, wantOK: true},
		{name: "recorded refusal", prompt: "hello", kind: "done", seq: 3, wantOK: true},
		{name: "recorded cancellation", prompt: "hello", kind: "cancelled", seq: 3, wantOK: true},
		{name: "input survives exit", prompt: "hello", kind: "input", seq: 3, data: map[string]any{"text": "hello"}, exit: true, wantOK: true},
		{name: "older input", prompt: "hello", kind: "input", seq: 2, data: map[string]any{"text": "hello"}},
		{name: "older input completed", prompt: "hello", kind: "input", seq: 2, data: map[string]any{"text": "hello"}, tail: []history.Entry{{Seq: 3, Kind: "done"}}},
		{name: "older refusal", prompt: "hello", kind: "done", seq: 2},
		{name: "different input", prompt: "hello", kind: "input", seq: 3, data: map[string]any{"text": "other"}},
		{name: "different input completed", prompt: "hello", kind: "input", seq: 3, data: map[string]any{"text": "other"}, tail: []history.Entry{{Seq: 4, Kind: "done"}}},
		{name: "different input cancelled", prompt: "hello", kind: "input", seq: 3, data: map[string]any{"text": "other"}, tail: []history.Entry{{Seq: 4, Kind: "cancelled"}}},
		{name: "typed expansion", prompt: "read @file", kind: "input", seq: 3, data: map[string]any{"text": "read @file\n\nexpanded attachment", "typed": "read @file"}, wantOK: true},
		{name: "multiline", prompt: "first\nsecond", kind: "input", seq: 3, data: map[string]any{"text": "first\nsecond"}, wantOK: true},
		{name: "rewritten engine input", prompt: "hello", kind: "input", seq: 3, data: map[string]any{"text": "rewritten", "typed": "hello"}, wantOK: true},
		{name: "meta is not input", prompt: "hello", kind: "meta", seq: 3},
		{name: "deadline", prompt: "hello"},
		{name: "child exit", prompt: "hello", exit: true},
		{name: "empty", wantOK: true},
		{name: "whitespace", prompt: " \t\n ", wantOK: true},
		{name: "slash command", prompt: "/model", wantOK: true},
		{name: "shell command", prompt: "!pwd", wantOK: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			f := newFixture(t)
			f.seed(t, "initial", append([]history.Entry{{Seq: tc.seq, Kind: tc.kind, Data: tc.data}}, tc.tail...)...)
			ch := &child{done: make(chan struct{})}
			if tc.exit {
				close(ch.done)
			}
			err := f.sup.waitCreatePrompt(ch, "initial", tc.prompt, 2, time.Now().Add(-time.Second))
			if (err == nil) != tc.wantOK {
				t.Fatalf("waitCreatePrompt = %v, want success %v", err, tc.wantOK)
			}
		})
	}
}

// Archive intentionally ends an unread first prompt, but keeps the
// visible session. The create waiter must not delete it as a failed boot.
func TestCreateArchiveSettlesUnreadPrompt(t *testing.T) {
	t.Parallel()
	hold := filepath.Join(t.TempDir(), "input")
	if err := os.WriteFile(hold, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	f := newFixture(t, envTurns+"=1", envHoldInput+"="+hold)
	id := history.NewID()
	answer := make(chan error, 1)
	go func() {
		_, err := f.sup.Create(CreateOptions{ID: id, Cwd: f.home, Prompt: "first prompt"})
		answer <- err
	}()
	waitFor(t, "the unread first prompt", func() bool {
		f.sup.mu.Lock()
		defer f.sup.mu.Unlock()
		ch := f.sup.kids[id]
		return ch != nil && ch.unread
	})
	if err := f.sup.SetArchived(id, true); err != nil {
		t.Fatal(err)
	}
	// Unarchive can race the waiter's next poll; it does not undo the
	// intentional end of this child's unread prompt.
	if err := f.sup.SetArchived(id, false); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-answer:
		if err != nil {
			t.Fatalf("intentional archive failed the create: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("archive did not settle the create")
	}
	entries, err := f.sup.Entries(id)
	if err != nil || len(entries) == 0 {
		t.Fatalf("archive lost the session history: %v %v", entries, err)
	}
	for _, e := range entries {
		if e.Kind == "input" {
			t.Fatal("the held child consumed the intentionally archived prompt")
		}
	}
	if f.sup.Live(id) {
		t.Fatal("archive left the first child live")
	}
}

func TestArchiveFailedSaveDoesNotAcknowledgeCreate(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "archive-save")
	if err := f.sup.SetTitle("archive-save", "kept"); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.Adopt("archive-save"); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(f.sup.opt.MetaPath+".tmp", 0o755); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.SetArchived("archive-save", true); err == nil {
		t.Fatal("archive succeeded with the metadata write blocked")
	}
	f.sup.mu.Lock()
	ch := f.sup.kids["archive-save"]
	ended := ch != nil && ch.archiveEnded
	f.sup.mu.Unlock()
	if ch == nil || ended || !f.sup.Live("archive-save") || f.sup.Meta("archive-save").Archived {
		t.Fatal("a failed archive changed the child or acknowledged its pending create")
	}
}
