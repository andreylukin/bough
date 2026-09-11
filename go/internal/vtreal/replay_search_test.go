package vtreal

// Transcript search over a 30-turn replay. The Go UI has no search
// action yet (no keymap entry, nothing in plugins/ui), so there is no
// match count, cue or next/prev to assert. What is asserted: the term
// from an early turn is reachable by scrolling, and ctrl+s / esc leave
// the view and the composer draft alone.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const searchTerm = "zanzibar"

// searchTape writes a 30-turn tape; only turn 3's reply holds searchTerm.
func searchTape(t *testing.T) (string, []string) {
	t.Helper()
	var b strings.Builder
	seq := 0
	emit := func(kind string, data map[string]string) {
		seq++
		line, _ := json.Marshal(map[string]any{"seq": seq, "at": "2026-09-10T10:00:00Z", "kind": kind, "data": data})
		b.Write(line)
		b.WriteByte('\n')
	}
	emit("meta", map[string]string{"cwd": "/tmp/demo"})
	var inputs []string
	for i := 1; i <= 30; i++ {
		in := fmt.Sprintf("question number %d", i)
		reply := fmt.Sprintf("answer number %d", i)
		if i == 3 {
			reply = "the capital token is " + searchTerm
		}
		inputs = append(inputs, in)
		emit("input", map[string]string{"text": in})
		emit("assistant", map[string]string{"text": "```stop\n" + reply + "\n```"})
		emit("done", map[string]string{"text": ""})
	}
	p := filepath.Join(t.TempDir(), "search.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p, inputs
}

func searchBoot(t *testing.T) *app {
	t.Helper()
	tape, inputs := searchTape(t)
	a := startCfg(t, 100, 30, replayConfig(tape))
	for i, in := range inputs {
		a.typeText(in)
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i+1, 30*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
		}
	}
	a.waitFor("answer number 30")
	a.check("after 30 turns")
	if strings.Contains(a.settled(), searchTerm) {
		t.Fatalf("%q already on screen; the tape is too short to test scrolling to it:\n%s", searchTerm, a.text())
	}
	return a
}

func TestSearch(t *testing.T) {
	t.Parallel()

	t.Run("TestSearchTermReachableByScroll", func(t *testing.T) {
		t.Parallel()
		a := searchBoot(t)
		for range 200 {
			if strings.Contains(a.settled(), searchTerm) {
				break
			}
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		}
		if !strings.Contains(a.settled(), searchTerm) {
			t.Fatalf("scrolling up never reached %q:\n%s", searchTerm, a.text())
		}
		a.check("at the match")
		// Paced: a burst of wheel events fills the PTY and blocks the write.
		for range 200 {
			if strings.Contains(a.text(), "answer number 30") {
				break
			}
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelDown})
			time.Sleep(20 * time.Millisecond)
		}
		a.waitFor("answer number 30")
		a.check("back at the bottom")
	})

	t.Run("TestSearchKeyAndEscKeepViewAndDraft", func(t *testing.T) {
		t.Parallel()
		a := searchBoot(t)
		a.typeText("draft text")
		a.waitFor("draft text")
		a.key('s', uv.ModCtrl)
		a.waitFor("esc closes") // the search bar replaced the status bar
		a.key(uv.KeyEscape, 0)
		// The esc hold (escresidue.go) releases the Esc 250 ms late: a
		// quiet screen before then is not the settled one.
		a.waitFor("? keys")
		s := a.settled()
		if !strings.Contains(s, "draft text") {
			t.Errorf("draft lost after ctrl+s / esc:\n%s", a.text())
		}
		if !strings.Contains(s, "answer number 30") {
			t.Errorf("view left the bottom after ctrl+s / esc:\n%s", a.text())
		}
		a.check("after ctrl+s / esc")
	})
}
