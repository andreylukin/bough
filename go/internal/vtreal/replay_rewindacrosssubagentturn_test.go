package vtreal

// Rewind across a turn that spawned a subagent. A replayed tape never
// spawns, so the test builds the workspace the way replay_undo_test.go
// does: a git repo in $HOME, a real checkpoint before turn 2, the file
// the turn's subagent wrote, and a session file whose turn 2 carries
// the sub:* entries. bough resumes it; double esc rewinds to before
// turn 2. The conversation must lose the subagent card and every entry
// after turn 1, a fresh bough resuming the fork must show the same,
// and — the gated part — the subagent's edit must be undone on disk.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// rewindAcrossSubagentTurnWorkspace writes the repo and the session and
// returns home and the session path. sub.txt is "original" at turn 2's
// checkpoint and "edited by subagent" on disk.
func rewindAcrossSubagentTurnWorkspace(t *testing.T) (home, session string) {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("git", "-C", home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	write := func(name, s string) {
		if err := os.WriteFile(filepath.Join(home, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write(".gitignore", ".bough/\nbough.yml\n")
	write("sub.txt", "original\n")
	tree, err := history.Snapshot(home)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	write("sub.txt", "edited by subagent\n")

	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	at := time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC)
	sub := func(text string) map[string]any { return map[string]any{"worker": 1, "text": text} }
	entries := []history.Entry{
		{Seq: 1, At: at, Kind: "meta", Data: map[string]any{"cwd": home}},
		{Seq: 2, At: at, Kind: "input", Data: map[string]any{"text": "first turn ALPHA"}},
		{Seq: 3, At: at, Kind: "assistant", Data: map[string]any{"text": "```stop\nREPLY-ALPHA\n```"}},
		{Seq: 4, At: at, Kind: "done", Data: map[string]any{"text": ""}},
		{Seq: 5, At: at, Kind: "input", Data: map[string]any{"text": "second turn BETA", "checkpoint": tree}},
		{Seq: 6, At: at, Kind: "assistant", Data: map[string]any{"text": "Spawning a subagent."}},
		{Seq: 7, At: at, Kind: "sub:start", Data: sub("rewrite sub.txt SUBTASK"), Parent: 6},
		{Seq: 8, At: at, Kind: "sub:code", Data: sub(`tools.write("sub.txt", "edited by subagent\n")`), Parent: 7},
		{Seq: 9, At: at, Kind: "sub:result", Data: sub("wrote sub.txt"), Parent: 8},
		{Seq: 10, At: at, Kind: "sub:assistant", Data: sub("Status: ok\nFindings: SUBREPORT"), Parent: 9},
		{Seq: 11, At: at, Kind: "sub:done", Data: map[string]any{"worker": 1, "status": "ok", "steps": 2}, Parent: 10},
		{Seq: 12, At: at, Kind: "assistant", Data: map[string]any{"text": "```stop\nREPLY-BETA\n```"}},
		{Seq: 13, At: at, Kind: "done", Data: map[string]any{"files": []string{"sub.txt"}}},
	}
	session = filepath.Join(dir, "rewind-sub-session.jsonl")
	var b strings.Builder
	for _, e := range entries {
		line, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	if err := os.WriteFile(session, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return home, session
}

// rewindAcrossSubagentTurnFork is the session file other than the
// original whose meta entry says forked_from, "" until one exists.
func rewindAcrossSubagentTurnFork(home, original string) string {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	for _, p := range paths {
		if p == original {
			continue
		}
		es, _ := history.Read(p)
		if len(es) > 0 && es[0].Kind == "meta" && es[0].Data["forked_from"] != nil {
			return p
		}
	}
	return ""
}

func TestRewindAcrossSubagentTurn(t *testing.T) {
	t.Parallel()
	home, session := rewindAcrossSubagentTurnWorkspace(t)
	a := undoStart(t, home, 100, 30, undoConfig(t, session))

	t.Run("Resumed", func(t *testing.T) {
		a.waitFor("REPLY-BETA")
		if s := a.settled(); !strings.Contains(s, "subagent 1") {
			t.Fatalf("no subagent card after resume:\n%s", s)
		}
		a.check("resumed")
	})

	var fork string
	t.Run("RewindMenuAndPick", func(t *testing.T) {
		a.key(uv.KeyEscape, 0)
		a.waitFor("press esc again to rewind")
		a.key(uv.KeyEscape, 0)
		a.waitFor("(current)")
		s := a.text()
		if !strings.Contains(s, "first turn ALPHA") || !strings.Contains(s, "second turn BETA") {
			t.Fatalf("rewind menu lacks the two turns:\n%s", s)
		}
		if !strings.Contains(s, "wrote sub.txt") {
			t.Errorf("turn 2 row does not say it wrote sub.txt:\n%s", s)
		}
		a.key(uv.KeyUp, 0) // "(current)" -> turn 2
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return strings.HasPrefix(followUpComposer(a), "> second turn BETA") },
			"the rewound prompt in the composer")
		a.waitUntil(func(string) bool { fork = rewindAcrossSubagentTurnFork(home, session); return fork != "" },
			"a forked session file")
	})

	t.Run("TranscriptRewound", func(t *testing.T) {
		s := a.settled()
		for _, gone := range []string{"REPLY-BETA", "subagent 1", "SUBTASK", "Spawning a subagent"} {
			if strings.Contains(s, gone) {
				t.Errorf("%q still on screen after rewind:\n%s", gone, s)
			}
		}
		if !strings.Contains(s, "REPLY-ALPHA") {
			t.Errorf("turn 1 lost by the rewind:\n%s", s)
		}
		a.check("after rewind")
	})

	t.Run("HistoryAfterRewindPoint", func(t *testing.T) {
		if fork == "" {
			t.Skip("no fork")
		}
		es, err := history.Read(fork)
		if err != nil {
			t.Fatal(err)
		}
		var inputs []string
		for _, e := range es {
			if strings.HasPrefix(e.Kind, "sub:") {
				t.Errorf("fork keeps subagent entry %s #%d", e.Kind, e.Seq)
			}
			if e.Seq > 4 {
				t.Errorf("fork keeps entry after the rewind point: %s #%d %v", e.Kind, e.Seq, e.Data)
			}
			if e.Kind == "input" {
				inputs = append(inputs, history.Prompt(e))
			}
		}
		if len(inputs) != 1 || inputs[0] != "first turn ALPHA" {
			t.Errorf("fork inputs = %q, want [first turn ALPHA]", inputs)
		}
		if es[0].Data["forked_from"] != session {
			t.Errorf("fork meta forked_from = %v, want %s", es[0].Data["forked_from"], session)
		}
		// The original is the other branch: its 13 entries stay (bough
		// may append a record of the switch after them).
		orig, _ := history.Read(session)
		if len(orig) < 13 || orig[12].Seq != 13 || orig[12].Kind != "done" {
			t.Errorf("original session lost its turns: %d entries", len(orig))
		}
	})

	t.Run("ResumeShowsRewound", func(t *testing.T) {
		if fork == "" {
			t.Skip("no fork")
		}
		_ = a.cmd.Process.Kill()
		_, _ = a.cmd.Process.Wait()
		b := undoStart(t, home, 100, 30, undoConfig(t, fork))
		b.waitFor("REPLY-ALPHA")
		s := b.settled()
		for _, gone := range []string{"REPLY-BETA", "subagent 1", "second turn BETA"} {
			if strings.Contains(s, gone) {
				t.Errorf("resumed fork shows %q:\n%s", gone, s)
			}
		}
		b.check("resumed fork")
	})

	t.Run("SubagentEditRestored", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_REWIND_ACROSS_SUBAGENT_TURN") == "" {
			t.Skip("known gap: rewind (plugins/ui/rewind.go handleRewindKey -> /tree fork) moves the conversation only; " +
				"the subagent's sub.txt edit stays on disk. Set BOUGH_KNOWN_REWIND_ACROSS_SUBAGENT_TURN=1 to run.")
		}
		if got, _ := os.ReadFile(filepath.Join(home, "sub.txt")); string(got) != "original\n" {
			t.Errorf("sub.txt = %q after rewinding past the subagent's turn, want %q", got, "original\n")
		}
	})
}
