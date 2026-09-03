package commands

// /undo and /tree: per-turn revert and session branching over the
// history service. A turn is an "input" entry (its seq names the
// turn; its "checkpoint" is the working tree before the model ran)
// closed by the next "done" entry (its "files" are what the turn
// wrote). /undo puts exactly those files back to the checkpoint and
// records an "undo" entry, so the next /undo walks one turn further
// back. /tree lists turns; "/tree <seq>" forks the session at one into
// a new file and resumes it through the session-choose seam. Like
// /model, the services are resolved lazily at Run time.

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// treeHistory is the slice of the history service /undo and /tree use.
type treeHistory interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
	Path() string
}

// turn is one user turn as the history records it.
type turn struct {
	seq        int64 // the input entry's seq
	at         time.Time
	text       string
	checkpoint string
	files      []string
	done       bool
	undone     bool
}

// turns folds the entries into turns, oldest first.
func turns(entries []history.Entry) []turn {
	var ts []turn
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text, _ := e.Data["text"].(string)
			cp, _ := e.Data["checkpoint"].(string)
			ts = append(ts, turn{seq: e.Seq, at: e.At, text: text, checkpoint: cp})
		case "done":
			if n := len(ts); n > 0 && !ts[n-1].done {
				ts[n-1].done = true
				ts[n-1].files = strList(e.Data["files"])
			}
		case "undo":
			seq, _ := intOf(e.Data["seq_of_turn"])
			for i := range ts {
				if ts[i].seq == seq {
					ts[i].undone = true
				}
			}
		}
	}
	return ts
}

// strList tolerates both the in-process ([]string) and JSONL-replayed
// ([]any) shapes of a done entry's files.
func strList(v any) []string {
	switch l := v.(type) {
	case []string:
		return l
	case []any:
		out := make([]string, 0, len(l))
		for _, x := range l {
			if s, ok := x.(string); ok {
				out = append(out, s)
			}
		}
		return out
	}
	return nil
}

// intOf reads a seq that may have round-tripped through JSON.
func intOf(v any) (int64, bool) {
	switch n := v.(type) {
	case int:
		return int64(n), true
	case int64:
		return n, true
	case float64:
		return int64(n), true
	}
	return 0, false
}

// registerTree installs /undo and /tree.
func registerTree(r *Registry, ctx *kernel.Context) error {
	if err := r.Register(
		CommandInfo{Name: "undo", Usage: "", Summary: "revert the files the last turn wrote"},
		func(string) (string, error) { return runUndo(ctx) },
	); err != nil {
		return err
	}
	return r.Register(
		CommandInfo{Name: "tree", Usage: "[seq]", Summary: "list the turns, or fork the session at one"},
		func(args string) (string, error) { return runTree(ctx, args) },
	)
}

// runUndo reverts the last completed turn not yet undone: only the
// files its done entry lists, back to the turn's checkpoint tree.
func runUndo(ctx *kernel.Context) (string, error) {
	h, err := kernel.Get[treeHistory](ctx, "history")
	if err != nil {
		return "", fmt.Errorf("undo: no history service")
	}
	ts := turns(h.Entries())
	i := len(ts) - 1
	for ; i >= 0; i-- {
		if ts[i].done && !ts[i].undone {
			break
		}
	}
	if i < 0 {
		return "", fmt.Errorf("undo: nothing to undo")
	}
	t := ts[i]
	var restored, skipped []string
	if len(t.files) > 0 {
		if t.checkpoint == "" {
			return "", fmt.Errorf("undo: turn %d has no checkpoint (not in a git repo)", t.seq)
		}
		cwd, err := os.Getwd()
		if err != nil {
			return "", err
		}
		if restored, skipped, err = history.Restore(cwd, t.checkpoint, t.files); err != nil {
			return "", fmt.Errorf("undo: %w", err)
		}
	}
	if restored == nil {
		restored = []string{}
	}
	h.Append("undo", map[string]any{"seq_of_turn": t.seq, "files": restored})
	noun := "files"
	if len(restored) == 1 {
		noun = "file"
	}
	var b strings.Builder
	fmt.Fprintf(&b, "reverted %d %s from turn %d", len(restored), noun, t.seq)
	for _, f := range restored {
		b.WriteString("\n  " + f)
	}
	for _, f := range skipped {
		b.WriteString("\n  " + f + " (outside the repo, left alone)")
	}
	return b.String(), nil
}

// treeLineMax caps a turn's first line in the /tree listing.
const treeLineMax = 60

// runTree lists the turns newest first, or forks at one.
func runTree(ctx *kernel.Context, args string) (string, error) {
	h, err := kernel.Get[treeHistory](ctx, "history")
	if err != nil {
		return "", fmt.Errorf("tree: no history service")
	}
	ts := turns(h.Entries())
	if args = strings.TrimSpace(args); args == "" {
		if len(ts) == 0 {
			return "tree: no turns yet", nil
		}
		var b strings.Builder
		for i := len(ts) - 1; i >= 0; i-- {
			t := ts[i]
			line := strings.SplitN(t.text, "\n", 2)[0]
			fmt.Fprintf(&b, "%4d  %s  %s", t.seq, t.at.Local().Format("15:04"), Ellipsize(line, treeLineMax))
			if !t.done {
				b.WriteString(" (running)")
			}
			b.WriteByte('\n')
		}
		b.WriteString("fork at a turn: /tree <seq>")
		return b.String(), nil
	}
	seq, err := strconv.ParseInt(args, 10, 64)
	if err != nil {
		return "", fmt.Errorf("usage: /tree [seq]")
	}
	src := h.Path()
	if src == "" {
		return "", fmt.Errorf("tree: this session has no history file to fork")
	}
	name := fmt.Sprintf("%s-%d-f%d", time.Now().UTC().Format(time.RFC3339), os.Getpid(), seq)
	if err := history.Fork(src, seq, filepath.Join(filepath.Dir(src), name+".jsonl")); err != nil {
		return "", fmt.Errorf("tree: %w", err)
	}
	return "", ResumeAction(name)
}
