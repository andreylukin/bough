package vtreal

// A session title off the llm-small tape that carries ESC, BEL, an
// embedded OSC 0 ("\x1b]0;PWNED\x07"), an SGR, U+202E (RTL override)
// and a newline. The title reaches three places: the OSC 0/2 tab
// title, the status bar, and the history "title" entry. None of them
// may pass the control bytes through raw: an ESC or BEL inside the OSC
// payload ends it early, and the rest of the "title" is a sequence the
// terminal runs (here: it renames the tab to PWNED).

import (
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// tabTitleOscWithControlCharsBad reports the first control or bidi
// override rune in s, "" when it is clean.
func tabTitleOscWithControlCharsBad(s string) string {
	for _, r := range s {
		if r < 0x20 || r == 0x7f || (r >= 0x80 && r < 0xa0) || (r >= 0x202a && r <= 0x202e) || (r >= 0x2066 && r <= 0x2069) {
			return string(r)
		}
	}
	return ""
}

// tabTitleOscWithControlCharsTitles records every distinct tab title
// seen while polling; the second func stops the poller.
func tabTitleOscWithControlCharsTitles(a *app) (func() []string, func()) {
	var mu sync.Mutex
	var seen []string
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			t := a.term.Snapshot().Title
			mu.Lock()
			if len(seen) == 0 || seen[len(seen)-1] != t {
				seen = append(seen, t)
			}
			mu.Unlock()
			select {
			case <-stop:
				return
			case <-time.After(5 * time.Millisecond):
			}
		}
	}()
	get := func() []string { mu.Lock(); defer mu.Unlock(); return append([]string(nil), seen...) }
	return get, func() { close(stop); <-done }
}

// tabTitleOscWithControlCharsHistory returns every "title" entry text
// under this run's $HOME.
func tabTitleOscWithControlCharsHistory(t *testing.T, home string) []string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	var titles []string
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range entries {
			if e.Kind == "title" {
				s, _ := e.Data["text"].(string)
				titles = append(titles, s)
			}
		}
	}
	return titles
}

func TestTabTitleOscWithControlChars(t *testing.T) {
	t.Parallel()
	main, small := llmSmallTapes(t, "tabtitleoscwithcontrolchars-small.jsonl")
	a := startCfg(t, 100, 30, llmSmallConfig(main, small, `
- id: session-title
  plugin: session-title
`))
	titles, stop := tabTitleOscWithControlCharsTitles(a)
	llmSmallTurn(a, "list the files here", 1)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "Fix") && strings.Contains(s, "flaky test") },
		"the session title on the status bar")
	screen := a.settled()
	time.Sleep(200 * time.Millisecond)
	stop()

	t.Run("osc payload sanitized", func(t *testing.T) {
		all := titles()
		for _, tt := range all {
			if strings.Contains(tt, "PWNED") {
				t.Errorf("an embedded OSC 0 renamed the tab: titles seen %q", all)
			}
			if b := tabTitleOscWithControlCharsBad(tt); b != "" {
				t.Errorf("tab title %q carries control rune %q", tt, b)
			}
		}
		last := all[len(all)-1]
		if !strings.Contains(last, "Fix") || !strings.Contains(last, "flaky test") {
			t.Errorf("final tab title %q lost the title text; seen %q", last, all)
		}
	})

	t.Run("status bar intact", func(t *testing.T) {
		ls := strings.Split(screen, "\n")
		bar := ls[len(ls)-1]
		for _, l := range ls { // the bar sits above the composer's row
			if strings.Contains(l, "? keys") {
				bar = l
			}
		}
		if !strings.Contains(bar, "? keys") || !strings.Contains(bar, "Fix") {
			t.Fatalf("status bar row broken: %q\n%s", bar, screen)
		}
		if b := tabTitleOscWithControlCharsBad(bar); b != "" {
			t.Errorf("status bar carries control rune %q: %q", b, bar)
		}
		a.check("titled")
	})

	t.Run("history consistent", func(t *testing.T) {
		hs := tabTitleOscWithControlCharsHistory(t, a.home)
		if len(hs) != 1 {
			t.Fatalf("history title entries = %q, want exactly one", hs)
		}
		if strings.Contains(hs[0], "second line") {
			t.Errorf("history title kept the second line: %q", hs[0])
		}
		// Raw or sanitized, but the same words the UI shows.
		joined := strings.Join(strings.Fields(strings.Map(func(r rune) rune {
			if tabTitleOscWithControlCharsBad(string(r)) != "" {
				return ' '
			}
			return r
		}, hs[0])), " ")
		if !strings.HasPrefix(joined, "Fix") || !strings.HasSuffix(joined, "flaky test") {
			t.Errorf("history title %q (words %q) is not the tape's title", hs[0], joined)
		}
	})
}
