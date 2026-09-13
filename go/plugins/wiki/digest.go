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
		if label, text, n, ok := describe(e); ok {
			digestLine(&b, e.Seq, label, text, n)
		}
	}
	return b.String()
}

// describe is how the digest names an entry and how much of it it
// keeps. ok is false for the kinds the digest skips: thinking, sub:* (a
// subagent's own steps; its report returns as a result), done, nudge,
// todo, job bookkeeping.
func describe(e history.Entry) (label, text string, maxLines int, ok bool) {
	text, _ = e.Data["text"].(string)
	switch e.Kind {
	case "meta":
		cwd, _ := e.Data["cwd"].(string)
		return "cwd", cwd, 1, cwd != ""
	case "title":
		return "title", text, 1, true
	case "input":
		return "user", text, 60, true
	case "steer":
		return "user (mid-turn)", text, 30, true
	case "assistant":
		return "assistant", text, 60, true
	case "code":
		return "ran", text, 8, true
	case "result":
		return "output", text, 10, true
	case "error":
		return "error", text, 10, true
	case "command":
		return "command", text, 3, true
	case "ask":
		return "asked the user", text, 10, true
	case "ask/answer":
		return "user answered", text, 10, true
	case "cancelled":
		return "cancelled", "the user stopped the turn", 1, true
	}
	return "", "", 0, false
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
