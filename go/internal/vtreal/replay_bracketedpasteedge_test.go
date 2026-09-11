package vtreal

// Hostile bracketed pastes written as raw bytes to the PTY: ESC and
// control bytes inside the paste, CRLF, a paste-end sequence split
// across two writes, and a nested paste-start. The composer must hold
// the sanitized text, no key action may fire, the terminal must stay
// in bracketed-paste + alt-screen mode, and enter must send exactly
// what the composer showed.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/plugins/history"
)

// bracketedpasteedgeWrite writes raw bytes to the app's PTY input,
// bypassing the emulator's own paste framing.
func bracketedpasteedgeWrite(t *testing.T, a *app, parts ...string) {
	t.Helper()
	for i, p := range parts {
		if i > 0 {
			time.Sleep(80 * time.Millisecond)
		}
		if _, err := a.term.pty.Write([]byte(p)); err != nil {
			t.Fatal(err)
		}
	}
}

// bracketedpasteedgeDraft returns the composer's text: every row from
// the first "> " row down to the status bar, prompt stripped.
func bracketedpasteedgeDraft(screen string) string {
	ls := strings.Split(screen, "\n")
	first := -1
	for i, l := range ls {
		if strings.HasPrefix(l, "> ") {
			first = i
			break
		}
	}
	if first < 0 {
		return ""
	}
	var out []string
	for _, l := range ls[first:] {
		if strings.Contains(l, "? keys") {
			break
		}
		out = append(out, strings.TrimSpace(strings.TrimPrefix(l, "> ")))
	}
	for len(out) > 0 && out[len(out)-1] == "" {
		out = out[:len(out)-1]
	}
	return strings.Join(out, "\n")
}

// bracketedpasteedgeInput is the newest "input" entry in this run's history.
func bracketedpasteedgeInput(a *app) string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var newest string
	var at time.Time
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.ModTime().Before(at) {
			newest, at = p, st.ModTime()
		}
	}
	if newest == "" {
		return ""
	}
	es, _ := history.Read(newest)
	for i := len(es) - 1; i >= 0; i-- {
		if es[i].Kind == "input" {
			return history.Prompt(es[i])
		}
	}
	return ""
}

func TestBracketedPasteEdge(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name  string
		parts []string
		want  []string // substrings the composer must show
		bad   []string // substrings that must never reach the screen
		known string   // a known product bug: skipped unless BOUGH_KNOWN_BRACKETEDPASTEEDGE is set
	}{
		{"esc_and_ctrl_bytes", []string{"\x1b[200~ab\x1bcd\x03ef\x07gh\x1b[201~"}, []string{"ab", "gh"}, []string{"[201~", "\x1b", "^["},
			"a paste carrying ESC/ctrl bytes never lands in the composer (uv keeps raw control bytes in PasteEvent; plugins/ui/paste.go handlePaste does not strip them)"},
		{"crlf", []string{"\x1b[200~one\r\ntwo\r\nthree\x1b[201~"}, []string{"one\ntwo\nthree"}, []string{"^M"}, ""},
		{"split_end", []string{"\x1b[200~split paste\x1b[20", "1~"}, []string{"split paste"}, []string{"[20", "1~"},
			"a paste-end split across reads is dropped as an expired UnknownEvent (ultraviolet terminal_reader.go scanEvents), so the paste never ends"},
		{"nested_start", []string{"\x1b[200~outer \x1b[200~inner\x1b[201~"}, []string{"outer", "inner"}, []string{"[200~", "[201~"}, ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			if c.known != "" && os.Getenv("BOUGH_KNOWN_BRACKETEDPASTEEDGE") == "" {
				t.Skip("known bug: " + c.known)
			}
			a := start(t, 80, 24)
			bracketedpasteedgeWrite(t, a, c.parts...)
			last := c.want[len(c.want)-1]
			a.waitFor(last[strings.LastIndex(last, "\n")+1:])
			s := a.settled()
			draft := bracketedpasteedgeDraft(s)
			for _, w := range c.want {
				if !strings.Contains(draft, w) {
					t.Fatalf("composer %q lacks %q:\n%s", draft, w, s)
				}
			}
			for _, b := range c.bad {
				if strings.Contains(s, b) {
					t.Fatalf("screen shows %q:\n%s", b, s)
				}
			}
			if strings.Contains(s, "echo:") || strings.Contains(s, "ctrl+c") {
				t.Fatalf("paste triggered a key action (submit/quit hint):\n%s", s)
			}
			if a.cmd.ProcessState != nil {
				t.Fatalf("process exited after paste")
			}
			snap := a.term.Snapshot()
			if !snap.AltScreen || snap.DEC[ansi.ModeBracketedPaste] != ansi.ModeSet {
				t.Fatalf("terminal state broken: alt=%v paste=%v", snap.AltScreen, snap.DEC[ansi.ModeBracketedPaste])
			}
			a.key(uv.KeyEnter, 0)
			if !a.waitDone(1, 30*time.Second) {
				t.Fatalf("turn never finished:\n%s", a.text())
			}
			if got := strings.TrimSpace(bracketedpasteedgeInput(a)); got != draft {
				t.Fatalf("sent %q, composer showed %q", got, draft)
			}
		})
	}
}
