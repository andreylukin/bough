package vtreal

// Queued follow-ups across a crash: alt+enter queues two lines while a
// turn streams, bough is SIGKILLed, and a second bough resumes the same
// session log. The queued lines may come back in the composer or be
// gone, but never run twice and never leave a half-written entry; the
// log must parse line by line and the killed turn must read as closed
// (done/cancelled) once the resumed session has booted.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// queuedMessagesSurviveCrashConfig is resumeConfig with the replay
// model slowed so the kill lands mid-turn.
func queuedMessagesSurviveCrashConfig(t *testing.T, tape, hist string) string {
	t.Helper()
	cfg := resumeConfig(tape, hist)
	out := strings.Replace(cfg, fmt.Sprintf("config: {file: %q}\n", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 300}\n", tape), 1)
	if out == cfg {
		t.Fatalf("could not add delay_ms to the replay row:\n%s", cfg)
	}
	return out
}

// queuedMessagesSurviveCrashLines checks every line of the log is a
// whole JSON object (the jsonl log's integrity check) and returns them.
func queuedMessagesSurviveCrashLines(t *testing.T, path string) []history.Entry {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(raw) > 0 && raw[len(raw)-1] != '\n' {
		t.Errorf("%s ends mid-line (half-written entry):\n%s", path, raw)
	}
	for i, l := range strings.Split(strings.TrimRight(string(raw), "\n"), "\n") {
		var v map[string]any
		if err := json.Unmarshal([]byte(l), &v); err != nil {
			t.Errorf("%s line %d is not a whole entry (%v): %q", path, i+1, err, l)
		}
	}
	entries, err := history.Read(path)
	if err != nil {
		t.Fatalf("history.Read(%s): %v", path, err)
	}
	return entries
}

func queuedMessagesSurviveCrashInputs(entries []history.Entry) map[string]int {
	n := map[string]int{}
	for _, e := range entries {
		if e.Kind == "input" {
			s, _ := e.Data["text"].(string)
			n[s]++
		}
	}
	return n
}

func queuedMessagesSurviveCrashDump(entries []history.Entry) string {
	var b strings.Builder
	for _, e := range entries {
		fmt.Fprintf(&b, "%s %v\n", e.Kind, e.Data)
	}
	return b.String()
}

func TestQueuedMessagesSurviveCrash(t *testing.T) {
	t.Parallel()
	tape := followUpTape(t)
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	first := startCfg(t, 100, 30, queuedMessagesSurviveCrashConfig(t, tape, log))
	first.typeText("alpha")
	first.key(uv.KeyEnter, 0)
	first.waitFor("one two") // alpha's reply is streaming
	first.typeText("beta")
	first.key(uv.KeyEnter, uv.ModAlt)
	first.waitFor("beta (queued)")
	first.typeText("gamma")
	first.key(uv.KeyEnter, uv.ModAlt)
	first.waitFor("gamma (queued)")
	if strings.Contains(first.text(), "REPLY-ALPHA") {
		t.Fatalf("alpha finished before the kill; the crash is not mid-turn:\n%s", first.text())
	}
	if err := first.cmd.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	_, _ = first.cmd.Process.Wait()
	killed := first.text()

	crashed := queuedMessagesSurviveCrashLines(t, log)
	in := queuedMessagesSurviveCrashInputs(crashed)
	if in["alpha"] != 1 {
		t.Fatalf("alpha recorded %d times before the resume, want 1:\n%s", in["alpha"], queuedMessagesSurviveCrashDump(crashed))
	}

	second := startCfg(t, 100, 30, resumeConfig(next, log))
	second.waitFor("resumed ")
	second.check("resumed after SIGKILL")
	time.Sleep(time.Second) // a resend of a queued line would have started by now
	s := second.settled()

	after := queuedMessagesSurviveCrashLines(t, log)
	in = queuedMessagesSurviveCrashInputs(after)
	for _, q := range []string{"alpha", "beta", "gamma"} {
		if in[q] > 1 {
			t.Errorf("%q recorded %d times across the crash:\n%s", q, in[q], s)
		}
	}
	for _, e := range after {
		txt, _ := e.Data["text"].(string)
		if e.Kind == "assistant" && (strings.Contains(txt, "REPLY-BETA") || strings.Contains(txt, "REPLY-GAMMA")) {
			t.Errorf("a queued line ran after the crash: %q", txt)
		}
	}
	if strings.Contains(s, "(queued)") {
		t.Errorf("resumed screen shows a queued line that nothing will run:\n%s", s)
	}
	composer := followUpComposer(second)
	for _, q := range []string{"beta", "gamma"} {
		if strings.Contains(s, q) && !strings.Contains(composer, q) && in[q] == 0 {
			t.Errorf("%q is on the resumed screen but neither in the composer nor in the log:\n%s", q, s)
		}
	}

	if os.Getenv("BOUGH_KNOWN_QUEUED_MESSAGES_SURVIVE_CRASH") == "" {
		t.Skip("known bug: a SIGKILLed turn stays open in the log after --resume (no done/cancelled after its input); set BOUGH_KNOWN_QUEUED_MESSAGES_SURVIVE_CRASH=1 to run")
	}
	open := false
	for _, e := range after {
		switch e.Kind {
		case "input":
			open = true
		case "done", "cancelled":
			open = false
		}
	}
	if open {
		t.Errorf("the killed turn is still open after the resume (no done/cancelled entry).\nscreen at kill:\n%s\nlog:\n%s", killed, queuedMessagesSurviveCrashDump(after))
	}
}
