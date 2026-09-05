package commands

// /export: writes this session's transcript to a Markdown file and
// tells the user where it landed — opencode ships the same command.
// The rendering mirrors what the UI shows (session.replay, model.go's
// render): a user/bough exchange per turn, executed code and its
// result as fenced blocks, everything else (asks, errors, notices) as
// a blockquote. Bookkeeping kinds that carry no story of their own
// (meta, done, nudge, subagent/todo detail) are left out.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// exportHistory is the slice of the history service /export needs.
type exportHistory interface {
	Entries() []history.Entry
	Path() string
}

// registerExport installs /export. Like /model and /tree, the history
// service is resolved lazily at Run time.
func registerExport(r *Registry, ctx *kernel.Context) error {
	return r.Register(
		CommandInfo{Name: "export", Usage: "[path]", Summary: "write this session's transcript to a Markdown file"},
		func(args string) (string, error) { return runExport(ctx, args) },
	)
}

// runExport renders the session and writes it to path (an explicit
// argument, else ~/.bough/exports/<session-id>.md), creating parent
// directories as needed.
func runExport(ctx *kernel.Context, args string) (string, error) {
	h, err := kernel.Get[exportHistory](ctx, "history")
	if err != nil {
		return "", fmt.Errorf("export: no history service — this session is not being recorded, so there is nothing to write: %w", err)
	}
	path := strings.TrimSpace(args)
	if path == "" {
		var perr error
		if path, perr = defaultExportPath(h.Path()); perr != nil {
			return "", fmt.Errorf("export: %w", perr)
		}
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return "", fmt.Errorf("export: %w", err)
	}
	md := renderMarkdown(h.Path(), h.Entries())
	if err := os.WriteFile(path, []byte(md), 0o644); err != nil {
		return "", fmt.Errorf("export: %w", err)
	}
	return "exported to " + path, nil
}

// defaultExportPath is ~/.bough/exports/<session-id>.md: a sibling of
// ~/.bough/attachments, named after the session so a re-export
// overwrites rather than piling up.
func defaultExportPath(sessPath string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	id := strings.TrimSuffix(filepath.Base(sessPath), ".jsonl")
	if id == "" || id == "." {
		id = time.Now().UTC().Format("20060102-150405")
	}
	return filepath.Join(home, ".bough", "exports", id+".md"), nil
}

// renderMarkdown is the whole file: a title (the session's own title
// entry, else its id), a one-line summary, then the entries in order.
func renderMarkdown(sessPath string, entries []history.Entry) string {
	id := strings.TrimSuffix(filepath.Base(sessPath), ".jsonl")
	title := ""
	for _, e := range entries {
		if e.Kind == "title" {
			if t, _ := e.Data["text"].(string); t != "" {
				title = t
			}
		}
	}
	if title == "" {
		title = "bough session " + id
	}
	var b strings.Builder
	fmt.Fprintf(&b, "# %s\n\n", title)
	if len(entries) > 0 {
		fmt.Fprintf(&b, "_session %s · %s · %d entries_\n\n", id, entries[0].At.Local().Format("2006-01-02 15:04"), len(entries))
	}
	for _, e := range entries {
		writeEntry(&b, e)
	}
	return strings.TrimRight(b.String(), "\n") + "\n"
}

// quoteBlock renders text as a Markdown blockquote, one "> " per line.
func quoteBlock(text string) string {
	return "> " + strings.ReplaceAll(strings.TrimRight(text, "\n"), "\n", "\n> ")
}

// writeEntry appends one entry's Markdown, or nothing for a kind
// that carries no reader-visible story of its own.
func writeEntry(b *strings.Builder, e history.Entry) {
	text, _ := e.Data["text"].(string)
	switch {
	case e.Kind == "meta" || e.Kind == "done" || e.Kind == "nudge" || e.Kind == "title":
		return // session bookkeeping, not the transcript
	case strings.HasPrefix(e.Kind, "sub:") || strings.HasPrefix(e.Kind, "todo/"):
		return // subagent and todo-list detail: too granular for a transcript
	}
	switch e.Kind {
	case "input":
		head := "## You"
		if steer, _ := e.Data["steer"].(bool); steer {
			head = "## You (steer)"
		}
		// What you typed: a reader of the transcript did not ask for
		// the skill that a word in it happened to inject.
		fmt.Fprintf(b, "%s\n\n%s\n\n", head, history.Prompt(e))
	case "assistant":
		fmt.Fprintf(b, "## bough\n\n%s\n\n", text)
	case "thinking":
		fmt.Fprintf(b, "<details><summary>thinking</summary>\n\n%s\n\n</details>\n\n", text)
	case "code":
		fmt.Fprintf(b, "```js\n%s\n```\n\n", strings.TrimRight(text, "\n"))
	case "result":
		fmt.Fprintf(b, "```\n%s\n```\n\n", strings.TrimRight(text, "\n"))
	case "ask":
		q, _ := e.Data["question"].(string)
		fmt.Fprintf(b, "**bough asks:** %s\n\n", q)
		for _, o := range strList(e.Data["options"]) {
			fmt.Fprintf(b, "- %s\n", o)
		}
		b.WriteByte('\n')
	case "ask/answer":
		fmt.Fprintf(b, "**you answer:** %s\n\n", text)
	case "cancelled":
		b.WriteString(quoteBlock("■ cancelled by you") + "\n\n")
	case "undo":
		files := strList(e.Data["files"])
		fmt.Fprintf(b, "%s\n\n", quoteBlock("↩ reverted: "+strings.Join(files, ", ")))
	case "error":
		b.WriteString(quoteBlock("⚠ "+text) + "\n\n")
	case "command":
		b.WriteString(quoteBlock("❯ "+text) + "\n\n")
	case "system", "job", "memory", "context":
		if text != "" {
			b.WriteString(quoteBlock(text) + "\n\n")
		}
	default:
		if text != "" {
			fmt.Fprintf(b, "%s\n\n", quoteBlock("["+e.Kind+"] "+text))
		}
	}
}
