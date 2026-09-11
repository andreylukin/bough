package vtreal

// Kitty CSI-u modifier keys editing a unicode draft (combining marks,
// a ZWJ emoji): after each key the composer text and the virtual
// cursor's position must match, and no ";5u" residue may reach the
// screen.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// kittyCSIuModifiersInComposerEditingStart boots bough with kitty
// advertised through TERM_PROGRAM (plus TERM, as kitty sets both).
func kittyCSIuModifiersInComposerEditingStart(t *testing.T) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 80, 24)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-kitty", "TERM_PROGRAM=kitty", "KITTY_WINDOW_ID=1",
		"COLORTERM=truecolor", "NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 80, rows: 24, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// kittyCSIuModifiersInComposerEditingSnap returns the composer row's
// draft (after "> ") and the draft text left of the reverse-video
// cursor cell; ok is false when no cursor cell is on that row.
func kittyCSIuModifiersInComposerEditingSnap(a *app) (draft, before string, ok bool) {
	snap := a.term.Snapshot()
	lines := a.lines()
	i := composerRow(lines)
	if i < 0 {
		return "", "", false
	}
	draft = strings.TrimPrefix(lines[i], "> ")
	var sb strings.Builder
	for x, c := range snap.Cells[i] {
		if x < 2 || c.Width == 0 {
			continue
		}
		if c.Style.Attrs&uv.AttrReverse != 0 {
			return draft, sb.String(), true
		}
		if c.Content == "" {
			sb.WriteByte(' ')
		} else {
			sb.WriteString(c.Content)
		}
	}
	return draft, strings.TrimRight(sb.String(), " "), false
}

// kittyCSIuModifiersInComposerEditingWant waits for the draft and the
// cursor to read as wanted, then checks for CSI residue.
func kittyCSIuModifiersInComposerEditingWant(t *testing.T, a *app, key, draft, before string) {
	t.Helper()
	a.waitUntil(func(string) bool {
		d, b, ok := kittyCSIuModifiersInComposerEditingSnap(a)
		return ok && d == draft && b == before
	}, key+": draft "+draft+" cursor after "+before)
	s := a.settled()
	d, b, ok := kittyCSIuModifiersInComposerEditingSnap(a)
	if !ok || d != draft || b != before {
		t.Fatalf("%s: draft=%q before-cursor=%q (cursor found %v), want %q / %q\n%s", key, d, b, ok, draft, before, s)
	}
	kittyKeyboardProtocolNoLeak(t, s)
}

func TestKittyCSIuModifiersInComposerEditing(t *testing.T) {
	t.Parallel()
	const (
		cafe  = "café"                 // e + combining acute
		emoji = "\U0001F469‍\U0001F4BB" // woman technologist, ZWJ sequence
		naive = "naïve"                // i + combining diaeresis
		draft = cafe + " " + emoji + " " + naive
	)
	a := kittyCSIuModifiersInComposerEditingStart(t)
	kittyKeyboardProtocolRaw(a, draft) // text arrives as UTF-8, as kitty sends it
	kittyCSIuModifiersInComposerEditingWant(t, a, "type", draft, draft)

	steps := []struct{ key, seq, draft, before string }{
		{"alt+b", "\x1b[98;3u", draft, cafe + " " + emoji + " "},
		{"alt+b", "\x1b[98;3u", draft, cafe + " "},
		{"alt+f", "\x1b[102;3u", draft, cafe + " " + emoji},
		{"ctrl+a", "\x1b[97;5u", draft, ""},
		{"alt+f", "\x1b[102;3u", draft, cafe},
		{"ctrl+e", "\x1b[101;5u", draft, draft},
		// The space before the deleted word stays (the row is trimmed,
		// the text left of the cursor is not).
		{"ctrl+w", "\x1b[119;5u", cafe + " " + emoji, cafe + " " + emoji + " "},
		{"ctrl+backspace", "\x1b[127;5u", cafe, cafe + " "},
	}
	for _, s := range steps {
		kittyKeyboardProtocolRaw(a, s.seq)
		kittyCSIuModifiersInComposerEditingWant(t, a, s.key, s.draft, s.before)
	}

	// shift+enter breaks the line without submitting; every composer
	// line carries "> ", so the last is "z" and the one above the draft.
	kittyKeyboardProtocolRaw(a, "\x1b[13;2u")
	kittyKeyboardProtocolType(a, "z")
	a.waitFor("z")
	s := a.settled()
	kittyKeyboardProtocolNoLeak(t, s)
	if strings.Contains(s, "echo:") {
		t.Fatalf("shift+enter submitted:\n%s", s)
	}
	lines := a.lines()
	i := composerRow(lines)
	if i < 1 || lines[i] != "> z" || lines[i-1] != "> "+cafe {
		t.Fatalf("shift+enter: want \"> %s\" then \"> z\":\n%s", cafe, s)
	}
}
