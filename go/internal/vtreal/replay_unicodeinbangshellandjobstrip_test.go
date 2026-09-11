package vtreal

// Unicode through the "!" shell and the background-job strip on a real
// PTY: RTL text, an emoji ZWJ family, wide CJK and raw control
// characters in a command's name and output. The strip must truncate
// on grapheme boundaries and by cells, control characters must never
// reach the terminal, and a copy of the "!" output must hand the
// clipboard tool the exact bytes the shell printed.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	unicodeinbangshellandjobstripRTL    = "שלום עולם"
	unicodeinbangshellandjobstripFamily = "👩\u200d👩\u200d👧\u200d👦"
	unicodeinbangshellandjobstripCJK    = "漢字テスト"
	unicodeinbangshellandjobstripKnown  = "BOUGH_KNOWN_UNICODE_IN_BANG_SHELL_AND_JOB_STRIP"
)

// unicodeinbangshellandjobstripStart boots yml with a fake pbcopy and
// xclip first on PATH: each writes its stdin to the returned file.
func unicodeinbangshellandjobstripStart(t *testing.T, cols, rows int, yml string) (*app, string) {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	fake := t.TempDir()
	clip := filepath.Join(home, "clip.out")
	for _, n := range []string{"pbcopy", "xclip"} {
		if err := os.WriteFile(filepath.Join(fake, n), []byte("#!/bin/sh\ncat > '"+clip+"'\n"), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "TMUX=", "WAYLAND_DISPLAY=",
		"PATH="+fake+string(os.PathListSeparator)+os.Getenv("PATH"),
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a, clip
}

func unicodeinbangshellandjobstripTape(t *testing.T) string {
	t.Helper()
	p, err := filepath.Abs("testdata/replay/bang-shell.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// unicodeinbangshellandjobstripCells is the screen with each cell's
// width: [content:width] for every non-blank cell. Failures carry it.
func unicodeinbangshellandjobstripCells(a *app) string {
	var sb strings.Builder
	for y, row := range a.term.Snapshot().Cells {
		fmt.Fprintf(&sb, "%2d|", y)
		for _, c := range row {
			if c.Content != "" && c.Content != " " {
				fmt.Fprintf(&sb, "[%q:%d]", c.Content, c.Width)
			}
		}
		sb.WriteByte('\n')
	}
	return sb.String()
}

// unicodeinbangshellandjobstripCheck is check with widths measured in
// cells (a ZWJ family is 7 runes in 2 cells, so a rune count lies):
// no row wider than the pane, no control character or U+FFFD in any
// cell, no OSC/CSI residue as text, and the title never hijacked.
func unicodeinbangshellandjobstripCheck(a *app, where string) {
	a.t.Helper()
	s := a.settled()
	if panicky.MatchString(s) {
		a.t.Errorf("%s: crash text on screen:\n%s", where, s)
	}
	ls := strings.Split(s, "\n")
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		a.t.Errorf("%s: composer not on the last rows (row %d of %d):\n%s", where, r, len(ls), s)
	}
	if !strings.Contains(s, "? keys") {
		a.t.Errorf("%s: status bar missing:\n%s", where, s)
	}
	snap := a.term.Snapshot()
	for y, row := range snap.Cells {
		w := 0
		for _, c := range row {
			w += c.Width
			if strings.ContainsFunc(c.Content, func(r rune) bool { return r < 0x20 || r == 0x7f || r == 0xfffd }) {
				a.t.Errorf("%s: row %d holds a control/replacement cell %q:\n%s", where, y, c.Content, s)
			}
		}
		if w > a.cols {
			a.t.Errorf("%s: row %d is %d cells wide in a %d-column pane:\n%s", where, y, w, a.cols, unicodeinbangshellandjobstripCells(a))
		}
	}
	// Output rows only (box rows and job rows): the command echo and
	// block labels show escapes as literal text, which is the point.
	for _, l := range ls {
		if !strings.HasPrefix(l, "│") && !strings.Contains(l, "job 1 · ") {
			continue
		}
		for _, bad := range []string{"]0;", "[31m", "TITLE-HIJACK"} {
			if strings.Contains(l, bad) {
				a.t.Errorf("%s: escape residue %q in output row %q:\n%s", where, bad, l, s)
			}
		}
	}
	if strings.Contains(snap.Title, "TITLE-HIJACK") {
		a.t.Errorf("%s: a command's OSC 0 reached the terminal: title %q", where, snap.Title)
	}
}

// unicodeinbangshellandjobstripNoPartialFamily fails when the screen
// shows a piece of the ZWJ family without the whole cluster.
func unicodeinbangshellandjobstripNoPartialFamily(a *app, where string) {
	a.t.Helper()
	s := a.settled()
	if rest := strings.ReplaceAll(s, unicodeinbangshellandjobstripFamily, ""); strings.ContainsAny(rest, "👩👧👦\u200d") {
		a.t.Errorf("%s: the ZWJ family was split mid-grapheme:\n%s\ncells:\n%s", where, s, unicodeinbangshellandjobstripCells(a))
	}
}

// unicodeinbangshellandjobstripJob boots on a tape whose one turn
// starts cmd as a background job, and returns its strip row.
func unicodeinbangshellandjobstripJob(t *testing.T, cols, rows int, cmd string) (*app, string) {
	t.Helper()
	code := fmt.Sprintf("tools.bash(%q, 120)\n", cmd)
	a, _ := unicodeinbangshellandjobstripStart(t, cols, rows, jobsConfig(bgjobsTape(t, code, 0)))
	jobsSay(a, "start the jobs")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 1 · ") }, "the strip to list job 1")
	for _, l := range strings.Split(a.settled(), "\n") {
		if strings.Contains(l, "job 1 · ") {
			return a, l
		}
	}
	t.Fatalf("job row vanished:\n%s", a.text())
	return nil, ""
}

// unicodeinbangshellandjobstripTail is the strip's " · <elapsed>" chip
// at the end of a job row.
var unicodeinbangshellandjobstripTail = regexp.MustCompile(` · \d+(s|m\d\ds)$`)

func TestUnicodeInBangShellAndJobStrip(t *testing.T) {
	t.Parallel()

	t.Run("BangOutputMixedScriptsAndCopyBytes", func(t *testing.T) {
		t.Parallel()
		a, clip := unicodeinbangshellandjobstripStart(t, 100, 30, replayConfig(unicodeinbangshellandjobstripTape(t)))
		mixed := "RTL " + unicodeinbangshellandjobstripRTL + " | ZWJ " + unicodeinbangshellandjobstripFamily +
			" | CJK " + unicodeinbangshellandjobstripCJK
		line := "!printf '" + mixed + "\\n\\033]0;TITLE-HIJACK\\007\\033[31mRED\\033[0m bell\\007 end\\n'"
		want := mixed + "\n\x1b]0;TITLE-HIJACK\x07\x1b[31mRED\x1b[0m bell\x07 end"
		bangShellRun(a, line, "bell end")
		s := a.settled()
		for _, part := range []string{unicodeinbangshellandjobstripRTL, unicodeinbangshellandjobstripFamily, unicodeinbangshellandjobstripCJK, "RED bell end"} {
			if !strings.Contains(s, part) {
				t.Errorf("output part %q missing or mangled:\n%s\ncells:\n%s", part, s, unicodeinbangshellandjobstripCells(a))
			}
		}
		unicodeinbangshellandjobstripCheck(a, "after mixed !printf")
		unicodeinbangshellandjobstripNoPartialFamily(a, "after mixed !printf")

		// Focus the result block by clicking its label, then copy it.
		x, y := -1, -1
		for i, l := range a.lines() {
			if j := strings.Index(l, "! printf"); j >= 0 {
				x, y = len([]rune(l[:j]))+1, i
			}
		}
		if y < 0 {
			t.Fatalf("result label not on screen:\n%s", a.text())
		}
		a.click(x, y)
		a.settled()
		a.key('x', uv.ModCtrl)
		a.waitFor("ctrl+x …")
		a.key('y', 0)
		a.waitFor("copied ")
		var got []byte
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			if b, err := os.ReadFile(clip); err == nil && len(b) > 0 {
				got = b
				break
			}
			time.Sleep(50 * time.Millisecond)
		}
		if string(got) != want {
			t.Errorf("clipboard got %q\nwant the original bytes %q", got, want)
		}
		unicodeinbangshellandjobstripCheck(a, "after copy")
	})

	t.Run("BangZWJRowKeepsBoxBorder", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(unicodeinbangshellandjobstripKnown) == "" {
			t.Skip("known bug: a ZWJ emoji row in a ! result box loses its right border (the frame's width for the cluster disagrees with the terminal's 2 cells); set " + unicodeinbangshellandjobstripKnown + "=1 to run")
		}
		a, _ := unicodeinbangshellandjobstripStart(t, 100, 30, replayConfig(unicodeinbangshellandjobstripTape(t)))
		bangShellRun(a, "!printf 'ZWJ "+unicodeinbangshellandjobstripFamily+" end\\n'", " end")
		for _, l := range strings.Split(a.settled(), "\n") {
			if strings.HasPrefix(l, "│") && !strings.HasSuffix(l, "│") {
				t.Errorf("box row lost its right border: %q\ncells:\n%s", l, unicodeinbangshellandjobstripCells(a))
			}
		}
	})

	t.Run("BangLongCJKRunWrapsByCells", func(t *testing.T) {
		t.Parallel()
		a, _ := unicodeinbangshellandjobstripStart(t, 50, 30, replayConfig(unicodeinbangshellandjobstripTape(t)))
		// "Z" alone would match the typed command; wait for the box row.
		bangShellRun(a, "!printf 'A'; printf '漢%.0s' $(seq 1 60); printf 'Z\\n'", "漢Z")
		s := a.settled()
		n := 0
		for _, l := range strings.Split(s, "\n") {
			if strings.HasPrefix(l, "│") {
				n += strings.Count(l, "漢")
			}
		}
		if n != 60 {
			t.Errorf("60 wide chars printed, %d on screen:\n%s\ncells:\n%s", n, s, unicodeinbangshellandjobstripCells(a))
		}
		unicodeinbangshellandjobstripCheck(a, "after long CJK !")
	})

	t.Run("JobStripZWJAtTruncationPoint", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(unicodeinbangshellandjobstripKnown) == "" {
			t.Skip("known bug: tools.firstLine (plugins/tools/jobs.go) cuts the job command at byte 80, splitting a ZWJ grapheme (strip shows \"👩\\u200d…\"); set " + unicodeinbangshellandjobstripKnown + "=1 to run")
		}
		// The family starts at byte 70: an 80-byte cut lands inside it.
		pre := "sleep 20; : "
		cmd := pre + strings.Repeat("x", 70-len(pre)) + unicodeinbangshellandjobstripFamily + " tail"
		a, row := unicodeinbangshellandjobstripJob(t, 100, 30, cmd)
		if !unicodeinbangshellandjobstripTail.MatchString(row) {
			t.Errorf("job row lost its elapsed tail: %q", row)
		}
		unicodeinbangshellandjobstripNoPartialFamily(a, "job strip with ZWJ")
		unicodeinbangshellandjobstripCheck(a, "job strip with ZWJ")
	})

	t.Run("JobStripWideCJKFitsPane", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(unicodeinbangshellandjobstripKnown) == "" {
			t.Skip("known bug: jobRows (plugins/ui/jobstrip.go) sizes line(r.Cmd, room) in graphemes, not cells, so wide CJK overflows the row and cuts its elapsed tail; set " + unicodeinbangshellandjobstripKnown + "=1 to run")
		}
		cmd := "sleep 20; : " + strings.Repeat("漢", 30)
		a, row := unicodeinbangshellandjobstripJob(t, 50, 24, cmd)
		if !unicodeinbangshellandjobstripTail.MatchString(row) {
			t.Errorf("wide job row overflowed, its elapsed tail is cut off: %q\ncells:\n%s", row, unicodeinbangshellandjobstripCells(a))
		}
		unicodeinbangshellandjobstripCheck(a, "job strip with CJK")
	})

	t.Run("JobStripRTLAndControls", func(t *testing.T) {
		t.Parallel()
		cmd := "sleep 20; : '" + unicodeinbangshellandjobstripRTL + " \x1b]0;TITLE-HIJACK\x07\x1b[31mRED\x07 " + unicodeinbangshellandjobstripCJK + "'"
		a, row := unicodeinbangshellandjobstripJob(t, 100, 30, cmd)
		for _, part := range []string{unicodeinbangshellandjobstripRTL, "RED", unicodeinbangshellandjobstripCJK} {
			if !strings.Contains(row, part) {
				t.Errorf("job row lost %q: %q", part, row)
			}
		}
		if !unicodeinbangshellandjobstripTail.MatchString(row) {
			t.Errorf("job row lost its elapsed tail: %q", row)
		}
		unicodeinbangshellandjobstripCheck(a, "job strip with RTL + controls")
	})
}
