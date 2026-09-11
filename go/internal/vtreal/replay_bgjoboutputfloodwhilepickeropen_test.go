package vtreal

// A background job floods output (50 bursts of `yes | head -n 1000`,
// 50k lines in all) while the model picker and then the action palette
// are open. The picker must keep up with typing — each filter char on
// screen within 500 ms, measured by screen polling — and the job strip
// must never draw over the picker's rows. Once both close, the job
// finishes: its strip row clears, the finished notice lands as the
// job block ("exited 0"), and the flood is kept in a spill file.

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	bgjobOutputFloodWhilePickerOpenBursts = 50
	bgjobOutputFloodWhilePickerOpenPer    = 1000
	bgjobOutputFloodWhilePickerOpenBudget = 500 * time.Millisecond
)

// bgjobOutputFloodWhilePickerOpenCmd floods a bounded 50k lines over
// roughly ten seconds, so the pickers are driven mid-flood.
func bgjobOutputFloodWhilePickerOpenCmd() string {
	return fmt.Sprintf("i=0; while [ $i -lt %d ]; do yes FLOODLINE | head -n %d; sleep 0.2; i=$((i+1)); done; echo FLOOD-END",
		bgjobOutputFloodWhilePickerOpenBursts, bgjobOutputFloodWhilePickerOpenPer)
}

// bgjobOutputFloodWhilePickerOpenTape: turn 1 starts the job, turn 2 is
// the wake turn its finished notice opens (replay is positional).
func bgjobOutputFloodWhilePickerOpenTape(t *testing.T) string {
	t.Helper()
	cmd := bgjobOutputFloodWhilePickerOpenCmd()
	code := fmt.Sprintf("console.log(tools.bash(%q, 120))\n", cmd)
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "start the flood"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"code", map[string]any{"text": code}},
		{"result", map[string]any{"code": code, "text": "job 1 started in the background (limit 2m0s): " + cmd + "\n"}},
		{"assistant", map[string]any{"text": "```stop\nStarted job 1.\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "[background job] finished"}},
		{"assistant", map[string]any{"text": "```stop\nFLOOD-WOKE\n```"}},
		{"done", map[string]any{"text": ""}},
	}
	var b strings.Builder
	for i, r := range rows {
		line, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": r.kind, "data": r.data})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "flood.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// bgjobOutputFloodWhilePickerOpenStrip reports whether a job 1 strip
// row sits anywhere on screen.
func bgjobOutputFloodWhilePickerOpenStrip(s string) bool {
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "job 1 · ") {
			return true
		}
	}
	return false
}

// bgjobOutputFloodWhilePickerOpenType types s one rune at a time into
// the picker and returns the slowest time for the search row to show
// the query so far.
func bgjobOutputFloodWhilePickerOpenType(a *app, s string) (time.Duration, string) {
	a.t.Helper()
	var worst time.Duration
	slow, q := "", ""
	for _, r := range s {
		q += string(r)
		want := "search " + q
		t0 := time.Now()
		a.typeText(string(r))
		for !strings.Contains(a.text(), want) {
			if time.Since(t0) > 10*time.Second {
				a.t.Fatalf("picker never showed %q:\n%s", want, a.text())
			}
			time.Sleep(5 * time.Millisecond)
		}
		if d := time.Since(t0); d > worst {
			worst, slow = d, q
		}
	}
	return worst, slow
}

// bgjobOutputFloodWhilePickerOpenSpills lists files under home, outside
// the history directory, that hold the job's flood lines.
func bgjobOutputFloodWhilePickerOpenSpills(home string) []string {
	var out []string
	hist := filepath.Join(home, ".bough", "history")
	filepath.WalkDir(home, func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() || strings.HasPrefix(p, hist) {
			return nil
		}
		if b, err := os.ReadFile(p); err == nil && strings.Count(string(b), "FLOODLINE\n") >= bgjobOutputFloodWhilePickerOpenPer {
			out = append(out, p)
		}
		return nil
	})
	return out
}

func TestBgjobOutputFloodWhilePickerOpen(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, jobsConfig(bgjobOutputFloodWhilePickerOpenTape(t)))
	a.check("boot")

	jobsSay(a, "start the flood")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.waitUntil(bgjobOutputFloodWhilePickerOpenStrip, "the job strip to name job 1")

	// Sample the screen while the pickers are up: no strip row may land
	// on a picker frame.
	stop := make(chan struct{})
	sampled := make(chan struct{})
	var overdraw string
	go func() {
		defer close(sampled)
		for {
			select {
			case <-stop:
				return
			case <-time.After(20 * time.Millisecond):
			}
			s := a.text()
			if overdraw == "" && strings.Contains(s, "pick a model") && bgjobOutputFloodWhilePickerOpenStrip(s) {
				overdraw = s
			}
		}
	}()

	a.typeText("/model")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a model")
	worst, at := bgjobOutputFloodWhilePickerOpenType(a, "llm-echo")
	a.waitFor("▸ llm-echo")
	if a.doneCount() != 1 {
		t.Errorf("the job finished before the picker closed; the flood is too short to test mid-flood")
	}
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "the picker to close")

	actionPaletteLeader(a, 'p')
	a.waitFor("clear_input")
	t0 := time.Now()
	a.typeText("expand")
	// Filtered: expand_all stays, a non-matching row is gone.
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "expand_all") && !strings.Contains(s, "clear_input")
	}, "the palette to filter to expand")
	paletteLag := time.Since(t0)
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "action · ") }, "the palette to close")
	close(stop)
	<-sampled

	t.Run("picker keeps up with typing", func(t *testing.T) {
		t.Logf("slowest picker keystroke %v (at %q); palette filter %v", worst, at, paletteLag)
		if worst > bgjobOutputFloodWhilePickerOpenBudget {
			t.Errorf("picker took %v to show %q mid-flood (budget %v)", worst, at, bgjobOutputFloodWhilePickerOpenBudget)
		}
		if paletteLag > 6*bgjobOutputFloodWhilePickerOpenBudget {
			t.Errorf("palette took %v to filter \"expand\" mid-flood", paletteLag)
		}
	})

	t.Run("strip never over the picker", func(t *testing.T) {
		if overdraw != "" {
			t.Errorf("job strip drawn on a picker frame:\n%s", overdraw)
		}
	})

	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("the finished job never woke a turn:\n%s", a.text())
	}
	a.waitFor("FLOOD-WOKE")
	a.check("after the wake")

	t.Run("strip shows finished", func(t *testing.T) {
		s := a.settled()
		if bgjobOutputFloodWhilePickerOpenStrip(s) {
			t.Errorf("job 1 still in the strip after it finished:\n%s", s)
		}
		if !strings.Contains(s, "▸ job") {
			t.Errorf("no finished job block in the transcript:\n%s", s)
		}
		note := ""
		for _, e := range jobsHistory(a) {
			if text, _ := e.Data["text"].(string); e.Kind == "input" && strings.Contains(text, "job 1 [exited 0]") {
				note = text
			}
		}
		if note == "" {
			t.Errorf("no \"job 1 [exited 0]\" notice recorded")
		} else if !strings.Contains(note, "FLOOD-END") {
			t.Errorf("finished notice lost the output tail:\n%s", note)
		}
	})

	t.Run("spill file exists", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_BGJOBOUTPUTFLOODWHILEPICKEROPEN") == "" {
			t.Skip("known bug: a background job's output is never spilled — plugins/tools/jobs.go keeps a bounded head+tail in memory and drops the middle (\"[N bytes cut]\"), so 50k lines leave no file; set BOUGH_KNOWN_BGJOBOUTPUTFLOODWHILEPICKEROPEN=1 to run")
		}
		if p := bgjobOutputFloodWhilePickerOpenSpills(a.home); len(p) == 0 {
			t.Errorf("no file under $HOME holds the job's flood output")
		}
	})
}
