package vtreal

// Copying a very large last reply (~120 KB) with ctrl+x y inside tmux.
// Two paths take the text (plugins/ui/clipboard.go): the OSC 52 write
// the app emits (one sequence, no chunking or size cap by design — the
// terminal decides) and `tmux load-buffer -`. The pane's raw output is
// captured with pipe-pane, so the OSC 52 payload is decoded and hashed
// against the tape's reply; `tmux show-buffer` must hold the same
// bytes; and none of the base64 may leak onto the screen.
//
// The app's PATH holds only tmux, so pbcopy/xclip are not found and
// the developer's real clipboard is left alone.

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

// copyOSC52OverTmuxLargeReply is the fixed generated reply: 2000 lines
// of 59 chars, deterministic, > 100 KB.
func copyOSC52OverTmuxLargeReply() string {
	var sb strings.Builder
	for i := range 2000 {
		fmt.Fprintf(&sb, "row %04d %s\n", i, strings.Repeat(string(rune('a'+i%26)), 50))
	}
	return strings.TrimRight(sb.String(), "\n")
}

func copyOSC52OverTmuxLargeTape(t *testing.T, reply string) string {
	t.Helper()
	entries := []struct {
		kind string
		data map[string]string
	}{
		{"meta", map[string]string{"cwd": "/tmp/demo"}},
		{"input", map[string]string{"text": "print a lot"}},
		{"assistant", map[string]string{"text": reply}},
		{"done", map[string]string{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-10T10:00:00Z", "kind": e.kind, "data": e.data})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "large.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// copyOSC52OverTmuxLargeStart boots bough in tmux with PATH = a dir
// holding only a tmux symlink.
func copyOSC52OverTmuxLargeStart(t *testing.T, yml string) (*tmuxApp, string) {
	t.Helper()
	tmuxBin, err := exec.LookPath("tmux")
	if err != nil {
		t.Skip("tmux not installed")
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	pathDir := t.TempDir()
	if err := os.Symlink(tmuxBin, filepath.Join(pathDir, "tmux")); err != nil {
		t.Fatal(err)
	}
	tm := &tmuxApp{t: t, sock: fmt.Sprintf("vtosc52-%d-%d", os.Getpid(), time.Now().UnixNano())}
	shell := fmt.Sprintf("cd %s && HOME=%s TERM=xterm-256color PATH=%s %s -config %s", home, home, pathDir, bin, cfg)
	tm.run("new-session", "-d", "-x", "100", "-y", "30", shell)
	t.Cleanup(func() {
		_ = exec.Command("tmux", "-L", tm.sock, "kill-server").Run()
		dir := os.Getenv("TMUX_TMPDIR")
		if dir == "" {
			dir = "/tmp"
		}
		_ = os.Remove(filepath.Join(dir, fmt.Sprintf("tmux-%d", os.Getuid()), tm.sock))
	})
	tm.waitFor("say something")
	return tm, home
}

// copyOSC52OverTmuxLargeSeq matches every OSC 52 write (BEL or ST end).
var copyOSC52OverTmuxLargeSeq = regexp.MustCompile(`\x1b\]52;([a-z]*);([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)`)

func copyOSC52OverTmuxLargeHash(s string) string { return fmt.Sprintf("%x", sha256.Sum256([]byte(s))) }

func TestCopyOSC52OverTmuxLarge(t *testing.T) {
	t.Parallel()
	reply := copyOSC52OverTmuxLargeReply()
	if len(reply) <= 100_000 {
		t.Fatalf("reply is only %d bytes", len(reply))
	}
	want := copyOSC52OverTmuxLargeHash(reply)
	tm, home := copyOSC52OverTmuxLargeStart(t, replayConfig(copyOSC52OverTmuxLargeTape(t, reply)))

	resizeTmuxSend(tm, "print a lot")
	resizeTmuxWaitDone(t, tm, home, 1)
	// The history "done" can land before the UI has streamed the whole
	// reply; copying then takes a prefix. Wait for the last row and for
	// the spinner (a braille glyph) to go.
	spinner := regexp.MustCompile(`[⠀-⣿] \d+s`)
	tm.waitUntil(func(s string) bool {
		return strings.Contains(s, "row 1999 ") && !spinner.MatchString(s)
	}, "the whole reply streamed")
	tm.settled()

	raw := filepath.Join(t.TempDir(), "pane.raw")
	tm.run("pipe-pane", "-o", "-t", "0", "cat > "+raw)
	tm.keys("C-x")
	tm.waitFor("ctrl+x")
	tm.keys("y")
	tm.waitFor("copied ")
	s := tm.settled()

	t.Run("flash", func(t *testing.T) {
		if !strings.Contains(s, "copied 2,000 lines") && !strings.Contains(s, "copied 2000 lines") {
			t.Errorf("flash should count 2000 lines:\n%s", s)
		}
		if !strings.Contains(s, "tmux buffer") || !strings.Contains(s, "OSC 52") {
			t.Errorf("flash should name tmux buffer and OSC 52:\n%s", s)
		}
	})

	t.Run("osc52_payload", func(t *testing.T) {
		var b []byte
		var ms [][]string
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			b, _ = os.ReadFile(raw)
			if ms = copyOSC52OverTmuxLargeSeq.FindAllStringSubmatch(string(b), -1); len(ms) > 0 {
				break
			}
			time.Sleep(100 * time.Millisecond)
		}
		if len(ms) == 0 {
			t.Fatalf("no complete OSC 52 sequence in %d raw bytes (has prefix: %v)", len(b), strings.Contains(string(b), "\x1b]52;"))
		}
		// As designed: one unchunked sequence carrying the whole text.
		if len(ms) != 1 {
			t.Errorf("want 1 OSC 52 sequence, got %d", len(ms))
		}
		dec, err := base64.StdEncoding.DecodeString(ms[0][2])
		if err != nil {
			t.Fatalf("payload is not base64: %v", err)
		}
		if got := copyOSC52OverTmuxLargeHash(string(dec)); got != want {
			t.Errorf("OSC 52 payload hash %s (%d bytes), want %s (%d bytes)", got, len(dec), want, len(reply))
		}
	})

	t.Run("tmux_buffer", func(t *testing.T) {
		buf := tm.run("show-buffer")
		if got := copyOSC52OverTmuxLargeHash(buf); got != want {
			t.Errorf("tmux buffer hash %s (%d bytes), want %s (%d bytes)", got, len(buf), want, len(reply))
		}
	})

	t.Run("no_garbage", func(t *testing.T) {
		if panicky.MatchString(s) {
			t.Errorf("crash text on screen:\n%s", s)
		}
		if strings.Contains(s, "]52;") || regexp.MustCompile(`[A-Za-z0-9+/]{60,}`).MatchString(s) {
			t.Errorf("OSC 52 bytes leaked onto the screen:\n%s", s)
		}
		ls := strings.Split(resizeTmuxScreen(tm), "\n")
		if r := composerRow(ls); r < 0 || r < len(ls)-3 {
			t.Errorf("composer not on the last rows:\n%s", s)
		}
		for i, l := range ls {
			if w := len([]rune(l)); w > 100 {
				t.Errorf("row %d is %d cells wide:\n%s", i, w, s)
			}
		}
	})
}
