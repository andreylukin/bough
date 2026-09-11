package vtreal

// A block result past the loop's cap spills to ~/.bough/spill, then
// esc lands while the next reply streams. The spill file must be
// complete and closed (its size is the whole result), the history
// result must end with the digest naming it, the transcript must show
// that digest, no half-streamed fence may be left behind, and a resume
// from the same log must name the same file.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// scratchSpillDuringCancelDigest matches the loop's digest line.
var scratchSpillDuringCancelDigest = regexp.MustCompile(`\[full output saved to (\S+) — (\d+) lines; use tools\.view or grep it\]`)

// scratchSpillDuringCancelTape: one block whose result is 105 kB, then
// a long reply that opens a js fence, so the cancel lands mid-fence.
func scratchSpillDuringCancelTape(t *testing.T) (tape, big string) {
	t.Helper()
	var sb strings.Builder
	for i := range 7000 {
		fmt.Fprintf(&sb, "log line %05d\n", i)
	}
	big = sb.String()
	code := "console.log(tools.bash(\"cat big.log\"))\n"
	long := "SPILLSTART\n```js\n// " + strings.Repeat("filler ", 300) + "\nSPILLEND\n```"
	entries := []map[string]any{
		{"kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"kind": "input", "data": map[string]any{"text": "dump the log"}},
		{"kind": "assistant", "data": map[string]any{"text": "```js\n" + code + "```"}},
		{"kind": "code", "data": map[string]any{"text": code}},
		{"kind": "result", "data": map[string]any{"code": code, "text": big}},
		{"kind": "assistant", "data": map[string]any{"text": long}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var out strings.Builder
	for i, e := range entries {
		e["seq"], e["at"] = i+1, "2026-09-10T10:00:00Z"
		b, _ := json.Marshal(e)
		out.Write(append(b, '\n'))
	}
	tape = filepath.Join(t.TempDir(), "spill-cancel.jsonl")
	if err := os.WriteFile(tape, []byte(out.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return tape, big
}

// scratchSpillDuringCancelConfig is resumeConfig with a streaming delay
// on the llm row, so the reply is still arriving when esc lands.
func scratchSpillDuringCancelConfig(tape, hist string, delayMS int) string {
	cfg := resumeConfig(tape, hist)
	out := strings.Replace(cfg, fmt.Sprintf("config: {file: %q}\n- id: codemode", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}\n- id: codemode", tape, delayMS), 1)
	if out == cfg {
		panic("scratchSpillDuringCancelConfig: resumeConfig changed shape")
	}
	return out
}

func scratchSpillDuringCancelEntries(path, kind string) []string {
	es, _ := history.Read(path)
	var out []string
	for _, e := range es {
		if e.Kind == kind {
			s, _ := e.Data["text"].(string)
			out = append(out, s)
		}
	}
	return out
}

// scratchSpillDuringCancelFindDigest expands the result block and pages
// down until the digest naming path is on screen.
func (a *app) scratchSpillDuringCancelFindDigest(path string) {
	a.t.Helper()
	row := hugeOutputRow(a.lines(), "▸ result")
	if row < 0 {
		a.t.Fatalf("no collapsed result header:\n%s", a.text())
	}
	a.click(2, row)
	a.waitFor("▾ result")
	for range 400 {
		if strings.Contains(a.settled(), path) {
			return
		}
		a.key(uv.KeyPgDown, 0)
	}
	a.t.Fatalf("digest naming %s never shown:\n%s", path, a.text())
}

func TestScratchSpillDuringCancel(t *testing.T) {
	t.Parallel()
	tape, big := scratchSpillDuringCancelTape(t)
	log := filepath.Join(t.TempDir(), "session.jsonl")
	a := startCfg(t, 240, 40, scratchSpillDuringCancelConfig(tape, log, 120))

	a.typeText("dump the log")
	a.key(uv.KeyEnter, 0)
	a.waitFor("SPILLSTART")
	a.key(uv.KeyEsc, 0)
	a.waitFor("■ cancelled")
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) }, "the spinner to stop")
	s := a.settled()
	if strings.Contains(s, "SPILLEND") {
		t.Fatalf("reply kept streaming after esc:\n%s", s)
	}
	if strings.Contains(s, "```") {
		t.Errorf("raw fence marker left on screen after the cancel:\n%s", s)
	}
	a.check("after cancel")

	// Exactly one spill file, closed and complete.
	spills, _ := filepath.Glob(filepath.Join(a.home, ".bough", "spill", "result-*.log"))
	if len(spills) != 1 {
		t.Fatalf("want one spill file, got %v", spills)
	}
	if st, err := os.Stat(spills[0]); err != nil || st.Size() != int64(len(big)) {
		t.Fatalf("spill file stat %v / %v, want %d bytes", st, err, len(big))
	}
	if b, _ := os.ReadFile(spills[0]); string(b) != big {
		t.Fatalf("spill file content differs from the result")
	}

	// The history result ends with the digest naming that file.
	var results []string
	a.waitUntil(func(string) bool {
		results = scratchSpillDuringCancelEntries(log, "result")
		return len(results) == 1
	}, "one result entry in history")
	m := scratchSpillDuringCancelDigest.FindStringSubmatch(results[0])
	if m == nil || !strings.HasSuffix(results[0], m[0]) {
		t.Fatalf("result entry does not end with a digest:\n…%s", results[0][max(0, len(results[0])-300):])
	}
	if m[1] != spills[0] || m[2] != "7000" {
		t.Fatalf("digest names %s (%s lines), want %s (7000 lines)", m[1], m[2], spills[0])
	}
	if n := len(scratchSpillDuringCancelEntries(log, "cancelled")); n != 1 {
		t.Errorf("want one cancelled entry in history, got %d", n)
	}
	for _, s := range scratchSpillDuringCancelEntries(log, "assistant") {
		if strings.Count(s, "```")%2 != 0 {
			t.Errorf("history kept a partial fence: %q", s[:min(len(s), 120)])
		}
	}
	a.scratchSpillDuringCancelFindDigest(spills[0])
	a.check("digest shown")

	// Resume from the same log: the same reference, no second spill.
	b := startCfg(t, 240, 40, resumeConfig(tape, log))
	b.waitFor("resumed ")
	b.scratchSpillDuringCancelFindDigest(spills[0])
	b.check("resumed digest")
	if again, _ := filepath.Glob(filepath.Join(a.home, ".bough", "spill", "result-*.log")); len(again) != 1 {
		t.Errorf("resume changed the spill files: %v", again)
	}
}
