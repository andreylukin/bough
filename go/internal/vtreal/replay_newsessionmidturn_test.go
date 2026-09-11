package vtreal

// /new while a turn is still streaming. The new session must start
// clean: no spinner or "working" chip, no cost/token chips carried from
// the old session, and no late delta of the old stream in the new pane.
// The old session's file must end its turn (done or cancelled) so a
// resume never finds a turn hanging open.
//
// Deterministic: the replay tape streams slowly (cancel.jsonl, ALPHA…
// filler…ALPHAEND), and the cost variant's fake provider holds its
// second reply until the test releases it. History is read after exit.

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// newSessionMidTurnSlash runs /new and waits for the old transcript to go.
func newSessionMidTurnSlash(a *app, gone string) {
	a.t.Helper()
	a.typeText("/new")
	a.waitFor("> /new")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, gone) }, "/new to clear the old transcript")
}

// newSessionMidTurnQuit exits with two ctrl+c so every file is flushed.
func newSessionMidTurnQuit(a *app) {
	a.t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done:
	case <-time.After(15 * time.Second):
		a.t.Fatalf("process did not exit after two ctrl+c:\n%s", a.text())
	}
}

// newSessionMidTurnFiles maps each history file to its entries.
func newSessionMidTurnFiles(t *testing.T, home string) map[string][]history.Entry {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	out := map[string][]history.Entry{}
	for _, p := range paths {
		es, err := history.Read(p)
		if err != nil {
			t.Fatalf("%s: %v", p, err)
		}
		out[p] = es
	}
	return out
}

func newSessionMidTurnKinds(es []history.Entry) []string {
	var ks []string
	for _, e := range es {
		ks = append(ks, e.Kind)
	}
	return ks
}

// newSessionMidTurnBar is the status bar row.
func newSessionMidTurnBar(s string) string {
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "? keys") {
			return l
		}
	}
	return ""
}

// newSessionMidTurnKnown skips while the known bug stands: /new swaps
// the session (session-choose remounts history -> loop -> ui) without
// cancelling the old loop's in-flight turn, so its later events land in
// the new pane — spinner, reply text and usage chips.
func newSessionMidTurnKnown(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_NEWSESSIONMIDTURN") == "" {
		t.Skip("known bug: /new mid-turn leaks the old stream (spinner, deltas, usage) into the new session; set BOUGH_KNOWN_NEWSESSIONMIDTURN=1 to run")
	}
}

func TestNewSessionMidTurn(t *testing.T) {
	t.Parallel()

	t.Run("Replay", func(t *testing.T) {
		t.Parallel()
		newSessionMidTurnKnown(t)
		// 40ms a word: ~200 words is ~8s of stream left after ALPHASTART.
		a := startCfg(t, 100, 30, cancelConfig(cancelTape(t), 40))
		a.typeText("start the long one")
		a.key(uv.KeyEnter, 0)
		a.waitFor("ALPHASTART")
		if !cancelSpinner.MatchString(a.text()) {
			t.Fatalf("no spinner while the reply streams:\n%s", a.text())
		}
		newSessionMidTurnSlash(a, "start the long one")

		// Watch past the old stream's natural end.
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			s := a.text()
			if strings.Contains(s, "filler") || strings.Contains(s, "ALPHA") {
				t.Fatalf("old stream's deltas reached the new pane:\n%s", s)
			}
			if cancelSpinner.MatchString(s) || strings.Contains(s, "working") {
				t.Fatalf("new session shows the old turn running:\n%s", s)
			}
			time.Sleep(50 * time.Millisecond)
		}
		a.check("new session")
		newSessionMidTurnQuit(a)

		files := newSessionMidTurnFiles(t, a.home)
		oldPath := ""
		for p, es := range files {
			for _, e := range es {
				if e.Kind == "input" && e.Data["text"] == "start the long one" {
					oldPath = p
				}
			}
		}
		if oldPath == "" {
			t.Fatalf("old session's input not persisted: %v", files)
		}
		ks := newSessionMidTurnKinds(files[oldPath])
		if l := ks[len(ks)-1]; l != "done" && l != "cancelled" {
			t.Errorf("old session does not end its turn: kinds %v", ks)
		}
		if len(files) < 2 {
			t.Errorf("/new made no second history file: %d files", len(files))
		}
		for p, es := range files {
			if p == oldPath {
				continue
			}
			for _, e := range es {
				if b, _ := json.Marshal(e.Data); strings.Contains(string(b), "ALPHA") || strings.Contains(string(b), "filler") {
					t.Errorf("%s: old stream written into another session: %v", p, e)
				}
			}
		}
	})

	t.Run("CostChipsReset", func(t *testing.T) {
		t.Parallel()
		newSessionMidTurnKnown(t)
		release := make(chan struct{})
		var once sync.Once
		var calls atomic.Int32
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path != "/v1/responses" {
				http.NotFound(w, r)
				return
			}
			text := "Done.\n\n```stop\nDone.\n```"
			w.Header().Set("Content-Type", "text/event-stream")
			if calls.Add(1) > 1 {
				// Paused: a first delta, then nothing until released.
				d, _ := json.Marshal(map[string]any{"type": "response.output_text.delta", "delta": "LATEPART "})
				fmt.Fprintf(w, "data: %s\n\n", d)
				w.(http.Flusher).Flush()
				select {
				case <-release:
				case <-r.Context().Done():
					return
				}
			}
			completed, _ := json.Marshal(map[string]any{
				"type": "response.completed",
				"response": map[string]any{
					"status": "completed",
					"output": []any{map[string]any{"type": "message",
						"content": []any{map[string]any{"type": "output_text", "text": text}}}},
					"usage": map[string]any{"input_tokens": costIn, "output_tokens": costOut},
				},
			})
			delta, _ := json.Marshal(map[string]any{"type": "response.output_text.delta", "delta": text})
			fmt.Fprintf(w, "data: %s\n\ndata: %s\n\ndata: [DONE]\n\n", delta, completed)
		}))
		t.Cleanup(func() {
			once.Do(func() { close(release) })
			srv.Close()
		})
		home, _ := costHome(t)
		os.Remove(filepath.Join(home, ".bough", "history", "costold.jsonl"))
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.costSend("first")
		a.costBar("after one turn", costMoney(costCall), "↑64.0k ↓2.0k")

		a.typeText("second")
		a.key(uv.KeyEnter, 0)
		a.waitFor("LATEPART")
		newSessionMidTurnSlash(a, "first")
		time.Sleep(time.Second)
		s := a.settled()
		bar := newSessionMidTurnBar(s)
		for _, stale := range []string{costMoney(costCall), "64.0k", "2.0k"} {
			if strings.Contains(bar, stale) {
				t.Errorf("new session status bar carries %q from the old session:\n%s", stale, s)
			}
		}
		if cancelSpinner.MatchString(s) {
			t.Errorf("new session shows the old turn running:\n%s", s)
		}
		once.Do(func() { close(release) })
		time.Sleep(2 * time.Second)
		s = a.settled()
		if strings.Contains(s, "LATEPART") || strings.Contains(s, "Done.") {
			t.Errorf("old stream's tail reached the new pane:\n%s", s)
		}
		bar = newSessionMidTurnBar(s)
		for _, stale := range []string{costMoney(2 * costCall), costMoney(costCall), "64.0k", "128.0k"} {
			if strings.Contains(bar, stale) {
				t.Errorf("old turn's usage landed on the new session's bar (%q):\n%s", stale, s)
			}
		}
		a.check("cost new session")
		newSessionMidTurnQuit(a)
		for p, es := range newSessionMidTurnFiles(t, home) {
			ks := newSessionMidTurnKinds(es)
			if slices.Contains(ks, "input") {
				if l := ks[len(ks)-1]; l != "done" && l != "cancelled" {
					t.Errorf("%s: session with a turn does not end it: %v", p, ks)
				}
			}
		}
	})
}
