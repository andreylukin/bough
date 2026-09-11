package wiki

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

// DigestFile renders the session's entries after seq from; see Digest.
func DigestFile(p paths, id string, from int64) (string, error) {
	if strings.ContainsAny(id, `/\`) || id == "" {
		return "", fmt.Errorf("digest: bad session id %q", id)
	}
	path := filepath.Join(p.hist, id+".jsonl")
	if _, err := os.Stat(path); err != nil {
		return "", fmt.Errorf("digest: no session %s", id)
	}
	entries, err := history.Read(path)
	if err != nil {
		return "", err
	}
	return Digest(id, entries, from), nil
}

// Digest is a session as the ingest agent reads it: what was asked,
// what was answered, what ran and what came back, each line headed by
// the entry's seq so a wiki page can cite `<id>#<seq>`. Tool output is
// cut short: the digest is for deciding what is worth keeping, and
// `bough log` still has every byte.
func Digest(id string, entries []history.Entry, from int64) string {
	var b strings.Builder
	fmt.Fprintf(&b, "session %s — entries after #%d (cite as `%s#<seq>`)\n\n", id, from, id)
	for _, e := range entries {
		if e.Seq <= from {
			continue
		}
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "meta":
			if cwd, _ := e.Data["cwd"].(string); cwd != "" {
				digestLine(&b, e.Seq, "cwd", cwd, 1)
			}
		case "title":
			digestLine(&b, e.Seq, "title", text, 1)
		case "input":
			digestLine(&b, e.Seq, "user", text, 60)
		case "steer":
			digestLine(&b, e.Seq, "user (mid-turn)", text, 30)
		case "assistant":
			digestLine(&b, e.Seq, "assistant", text, 60)
		case "code":
			digestLine(&b, e.Seq, "ran", text, 8)
		case "result":
			digestLine(&b, e.Seq, "output", text, 10)
		case "error":
			digestLine(&b, e.Seq, "error", text, 10)
		case "command":
			digestLine(&b, e.Seq, "command", text, 3)
		case "ask":
			digestLine(&b, e.Seq, "asked the user", text, 10)
		case "ask/answer":
			digestLine(&b, e.Seq, "user answered", text, 10)
		case "cancelled":
			digestLine(&b, e.Seq, "cancelled", "the user stopped the turn", 1)
		}
		// Skipped: thinking, sub:* (a subagent's own steps; its report
		// returns as a result), done, nudge, todo, job bookkeeping.
	}
	return b.String()
}

const maxLineChars = 400

func digestLine(b *strings.Builder, seq int64, label, text string, maxLines int) {
	text = strings.TrimRight(text, "\n")
	if strings.TrimSpace(text) == "" {
		return
	}
	lines := strings.Split(text, "\n")
	extra := 0
	if len(lines) > maxLines {
		extra = len(lines) - maxLines
		lines = lines[:maxLines]
	}
	for i, l := range lines {
		if r := []rune(l); len(r) > maxLineChars {
			lines[i] = string(r[:maxLineChars]) + "…"
		}
	}
	fmt.Fprintf(b, "#%d %s: %s\n", seq, label, strings.Join(lines, "\n    "))
	if extra > 0 {
		fmt.Fprintf(b, "    … %d more lines\n", extra)
	}
}
