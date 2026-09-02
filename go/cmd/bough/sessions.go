// Session resume: `bough sessions`, the -c/--continue and -r/--resume
// flags, and the launcher side of the session-picker seam ("sessions" +
// "session-picker" + "session-choose" services read by the ui plugin).
package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"text/tabwriter"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// sessionsDir is $HOME/.bough/history — where the history plugin
// writes one JSONL per session.
func sessionsDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		fatal(fmt.Errorf("home dir: %w", err))
	}
	return filepath.Join(home, ".bough", "history")
}

// runSessions is `bough sessions`: list stored sessions, newest first.
func runSessions(args []string) {
	if len(args) > 0 {
		fmt.Fprintln(os.Stderr, "usage: bough sessions   (no arguments; lists ~/.bough/history newest first)")
		if args[0] == "-h" || args[0] == "--help" || args[0] == "-help" {
			return
		}
		os.Exit(2)
	}
	dir := sessionsDir()
	infos, err := history.List(dir)
	if err != nil {
		fatal(err)
	}
	if len(infos) == 0 {
		fmt.Fprintf(os.Stderr, "bough: no sessions in %s\n", dir)
		return
	}
	printSessions(os.Stdout, infos)
}

// printSessions renders the session table: id, local time, entry
// count, first-input title truncated to ~60 columns.
func printSessions(w io.Writer, infos []history.SessionInfo) {
	tw := tabwriter.NewWriter(w, 2, 8, 2, ' ', 0)
	for _, in := range infos {
		title := strings.SplitN(in.Title, "\n", 2)[0]
		if r := []rune(title); len(r) > 60 {
			title = string(r[:59]) + "…"
		}
		fmt.Fprintf(tw, "%s\t%s\t%d entries\t%s\n",
			in.ID, in.ModTime.Local().Format("2006-01-02 15:04"), in.Entries, title)
	}
	tw.Flush()
}

// extractSessionFlags pulls -c/--continue and -r/--resume [id] out of
// args before the flag package sees them (flag can't do an optional
// value). "-r <id>", bare "-r", "-r=<id>" and the long forms all work.
func extractSessionFlags(args []string) (cont, resume bool, id string, rest []string) {
	for i := 0; i < len(args); i++ {
		a := args[i]
		switch {
		case a == "-c" || a == "--continue":
			cont = true
		case a == "-r" || a == "--resume":
			resume = true
			if i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
				i++
				id = args[i]
			}
		case strings.HasPrefix(a, "--resume="):
			resume = true
			id = strings.TrimPrefix(a, "--resume=")
		case strings.HasPrefix(a, "-r="):
			resume = true
			id = strings.TrimPrefix(a, "-r=")
		default:
			rest = append(rest, a)
		}
	}
	return
}

// resolveSession turns the session flags into either a concrete file
// to resume (returned path, "" = fresh session) or, for a bare
// --resume, a picker request (needPicker). It may exit: --resume with
// an unknown id exits 1 with near matches; bare --resume in headless
// mode prints the session list and exits 2.
func resolveSession(cont, resume bool, id, mode string) (resumePath string, needPicker bool) {
	if cont && resume {
		fatal(fmt.Errorf("use either --continue or --resume, not both"))
	}
	dir := sessionsDir()
	switch {
	case cont:
		infos, err := history.List(dir)
		if err != nil {
			fatal(err)
		}
		if len(infos) == 0 {
			fmt.Fprintln(os.Stderr, "bough: no previous session, starting fresh")
			return "", false
		}
		return infos[0].Path, false
	case resume && id != "":
		id = strings.TrimSuffix(id, ".jsonl")
		p := filepath.Join(dir, id+".jsonl")
		if _, err := os.Stat(p); err != nil {
			fmt.Fprintf(os.Stderr, "bough: no session %q in %s\n", id, dir)
			infos, _ := history.List(dir)
			near := 0
			for _, in := range infos {
				if strings.Contains(in.ID, id) {
					if near == 0 {
						fmt.Fprintln(os.Stderr, "did you mean:")
					}
					fmt.Fprintf(os.Stderr, "  %s\n", in.ID)
					near++
				}
			}
			if near == 0 {
				fmt.Fprintln(os.Stderr, "run `bough sessions` to list them")
			}
			os.Exit(1)
		}
		return p, false
	case resume:
		if mode == "headless" {
			infos, err := history.List(dir)
			if err != nil {
				fatal(err)
			}
			fmt.Fprintln(os.Stderr, "bough: --resume needs a session id in headless mode; sessions:")
			printSessions(os.Stdout, infos)
			os.Exit(2)
		}
		return "", true
	}
	return "", false
}

// providePicker wires the launcher side of the picker seam for a bare
// --resume in tui/web mode: the mount proceeds with a fresh session
// underneath the picker, and "session-choose" swaps the history row to
// the picked file via runtimeSet (Reconcile remounts history -> loop ->
// ui; the kernel's Get-tracking makes that cascade automatic, and the
// override is recorded so a config hot reload keeps the resumed file).
func providePicker(ctx *kernel.Context, src configSource, ov *overrides) {
	infos, err := history.List(sessionsDir())
	if err != nil {
		fatal(err)
	}
	ctx.Provide("sessions", infos)
	ctx.Provide("session-picker", "pending")
	ctx.Provide("session-choose", func(id string) {
		if id == "" {
			return // fresh session already mounted
		}
		set := "history.file=" + filepath.Join(sessionsDir(), id+".jsonl")
		if err := runtimeSet(ctx, src, ov, set); err != nil {
			fmt.Fprintln(os.Stderr, "bough: resume:", err)
		}
	})
}
