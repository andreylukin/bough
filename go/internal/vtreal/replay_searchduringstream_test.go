package vtreal

// ^s transcript search opened while a long reply is still streaming.
// The paced tape streams w001..w120 then a LATEWORD only the tail
// carries; the query targets LATEWORD, so any match index is computed
// before the text it points at exists. After enter / ctrl+n / esc and
// the turn finishing: no crash, no search overlay left over, the
// finished reply identical to a no-search golden run, and a composer
// draft typed before search opened is intact.
//
// The Go UI has no search action yet (no ctrl+s binding, nothing in
// plugins/ui), so the query/enter/next/esc run is
// gated behind BOUGH_KNOWN_SEARCHDURINGSTREAM.

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

const searchDuringStreamLate = "LATEWORDzq"

func searchDuringStreamTape(t *testing.T) string {
	t.Helper()
	var words []string
	for i := 1; i <= 120; i++ {
		words = append(words, fmt.Sprintf("w%03d", i))
	}
	reply := strings.Join(words, " ") + " " + searchDuringStreamLate + " fin."
	var b strings.Builder
	for i, e := range []struct {
		kind string
		data map[string]string
	}{
		{"meta", map[string]string{"cwd": "/tmp/demo"}},
		{"input", map[string]string{"text": "stream something"}},
		{"assistant", map[string]string{"text": reply}},
		{"done", map[string]string{"text": ""}},
	} {
		line, _ := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": e.kind, "data": e.data})
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "searchduringstream.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// searchDuringStreamReply is the reply's words on screen, in order.
func searchDuringStreamReply(s string) []string {
	var out []string
	for _, w := range strings.Fields(s) {
		if (len(w) == 4 && w[0] == 'w' && w[1] >= '0' && w[1] <= '9') || w == searchDuringStreamLate {
			out = append(out, w)
		}
	}
	return out
}

// searchDuringStreamRun streams the reply with a draft typed on the
// first delta, runs during() while the tail is still coming, then
// waits for the turn to finish.
func searchDuringStreamRun(t *testing.T, during func(a *app)) (a *app, final string) {
	t.Helper()
	a = startCfg(t, 100, 60, cancelConfig(searchDuringStreamTape(t), 40))
	a.typeText("stream something")
	a.key(uv.KeyEnter, 0)
	a.waitFor("w001")
	a.typeText("keep draft")
	a.waitFor("keep draft")
	if during != nil {
		if strings.Contains(a.text(), searchDuringStreamLate) {
			t.Fatalf("the reply finished before search opened; the tape is not paced:\n%s", a.text())
		}
		during(a)
	}
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("fin.")
	return a, a.settled()
}

// searchDuringStreamAfter is what every search run must leave behind.
func searchDuringStreamAfter(t *testing.T, a *app, s string, golden []string) {
	t.Helper()
	a.check("after search during stream")
	if got := searchDuringStreamReply(s); strings.Join(got, " ") != strings.Join(golden, " ") {
		t.Errorf("reply differs from the no-search golden\n got: %v\nwant: %v\nscreen:\n%s", got, golden, s)
	}
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(strings.ToLower(l), "search") {
			t.Errorf("search overlay still up: %q\n%s", l, s)
		}
	}
	ls := strings.Split(s, "\n")
	if r := composerRow(ls); r < 0 || !strings.Contains(ls[r], "keep draft") || strings.Contains(ls[r], searchDuringStreamLate) {
		t.Errorf("composer draft changed (row %d):\n%s", r, s)
	}
}

func TestSearchDuringStream(t *testing.T) {
	t.Parallel()
	var golden []string
	t.Run("golden", func(t *testing.T) {
		_, s := searchDuringStreamRun(t, nil)
		golden = searchDuringStreamReply(s)
		if len(golden) != 121 {
			t.Fatalf("golden reply has %d words, want 121:\n%s", len(golden), s)
		}
	})
	if golden == nil {
		t.FailNow()
	}

	// ctrl+s alone mid-stream must not disturb the stream or the draft.
	t.Run("ctrl_s_harmless", func(t *testing.T) {
		t.Parallel()
		a, s := searchDuringStreamRun(t, func(a *app) { a.key('s', uv.ModCtrl) })
		searchDuringStreamAfter(t, a, s, golden)
	})

	// The full scenario: query for text not yet streamed, enter, next, esc.
	t.Run("query_enter_next_esc", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_SEARCHDURINGSTREAM") == "" {
			t.Skip("known gap: the Go UI has no ^s transcript search; the query lands in the composer, enter sends it as a steer and esc cancels the turn (set BOUGH_KNOWN_SEARCHDURINGSTREAM=1 to run)")
		}
		var during string
		a, s := searchDuringStreamRun(t, func(a *app) {
			a.key('s', uv.ModCtrl)
			a.typeText(searchDuringStreamLate)
			during = a.settled()
			a.key(uv.KeyEnter, 0)
			a.key('n', uv.ModCtrl)
			a.key(uv.KeyEscape, 0)
		})
		if !strings.Contains(strings.ToLower(during), "search") {
			t.Errorf("ctrl+s mid-stream showed no search overlay:\n%s", during)
		}
		searchDuringStreamAfter(t, a, s, golden)
	})
}
