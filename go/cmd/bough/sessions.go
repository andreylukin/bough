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

// cwd is the working directory sessions are matched against ("" when
// unknown: then nothing matches and every listing is global).
func cwd() string {
	d, _ := os.Getwd()
	return d
}

// runSessions is `bough sessions [--all]`: list this directory's
// sessions, newest first; --all (or no session here) lists every
// session, this directory's first, with a cwd column.
func runSessions(args []string) {
	all := false
	for _, a := range args {
		if a == "-h" || a == "--help" || a == "-help" {
			fmt.Fprintln(os.Stderr, "usage: bough sessions [--all]   (this directory's sessions; --all lists every session, cwd first)")
			return
		}
		if a != "--all" && a != "-a" {
			fmt.Fprintf(os.Stderr, "usage: bough sessions [--all]; got %v\n", args)
			os.Exit(2)
		}
		all = true
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
	here := cwd()
	mine := forCwd(infos, here)
	if !all && len(mine) == 0 {
		fmt.Fprintf(os.Stderr, "bough: no sessions for this directory; showing all %d\n", len(infos))
		all = true
	}
	if all {
		printSessions(os.Stdout, history.PreferCwd(infos, here), true)
		return
	}
	printSessions(os.Stdout, mine, false)
}

// forCwd filters infos to the sessions recorded in dir.
func forCwd(infos []history.SessionInfo, dir string) []history.SessionInfo {
	var out []history.SessionInfo
	for _, in := range infos {
		if in.Cwd == dir {
			out = append(out, in)
		}
	}
	return out
}

// printSessions renders the session table: id, local time, entry
// count, first-input title truncated to ~60 columns, and with withCwd
// the recorded working directory ("?" for files predating it).
func printSessions(w io.Writer, infos []history.SessionInfo, withCwd bool) {
	tw := tabwriter.NewWriter(w, 2, 8, 2, ' ', 0)
	for _, in := range infos {
		title, _, _ := strings.Cut(in.Title, "\n")
		if r := []rune(title); len(r) > 60 {
			title = string(r[:59]) + "…"
		}
		fmt.Fprintf(tw, "%s\t%s\t%d entries\t%s",
			in.ID, in.ModTime.Local().Format("2006-01-02 15:04"), in.Entries, title)
		if withCwd {
			d := in.Cwd
			if d == "" {
				d = "?"
			}
			fmt.Fprintf(tw, "\t%s", d)
		}
		fmt.Fprintln(tw)
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
		// Sessions are global across projects: prefer this directory's.
		if mine := forCwd(infos, cwd()); len(mine) > 0 {
			return mine[0].Path, false
		}
		fmt.Fprintln(os.Stderr, "bough: no session for this directory, resuming newest")
		return infos[0].Path, false
	case resume && id != "":
		id = strings.TrimSuffix(id, ".jsonl")
		p := filepath.Join(dir, id+".jsonl")
		if _, err := os.Stat(p); err != nil {
			infos, _ := history.List(dir)
			// Session ids are UUIDv7s now: nobody types one in full,
			// so an unambiguous prefix resumes. Ambiguous or unknown
			// falls through to the suggestions below.
			var pre []history.SessionInfo
			for _, in := range infos {
				if strings.HasPrefix(in.ID, id) {
					pre = append(pre, in)
				}
			}
			if len(pre) == 1 {
				return pre[0].Path, false
			}
			if len(pre) > 1 {
				fmt.Fprintf(os.Stderr, "bough: %q matches %d sessions in %s:\n", id, len(pre), dir)
				for _, in := range pre {
					fmt.Fprintf(os.Stderr, "  %s\n", in.ID)
				}
				os.Exit(1)
			}
			fmt.Fprintf(os.Stderr, "bough: no session %q in %s\n", id, dir)
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
			printSessions(os.Stdout, history.PreferCwd(infos, cwd()), true)
			os.Exit(2)
		}
		return "", true
	}
	return "", false
}

// providePicker wires the launcher side of the picker seam for a bare
// --resume in tui/web mode: the mount proceeds with a fresh session
// underneath the picker, and the session list (this directory's
// sessions first) is provided for it. provideChoose supplies the swap.
func providePicker(ctx *kernel.Context) {
	infos, err := history.List(sessionsDir())
	if err != nil {
		fatal(err)
	}
	ctx.Provide("sessions", history.PreferCwd(infos, cwd()))
	ctx.Provide("session-picker", "pending")
}

// provideChoose wires "session-choose", which swaps the history row to
// the picked file via runtimeSet (Reconcile remounts history -> loop ->
// ui; the kernel's Get-tracking makes that cascade automatic, and the
// override is recorded so a config hot reload keeps the resumed file).
// Always provided: the ui's /sessions picker resumes mid-session too.
func provideChoose(ctx *kernel.Context, src configSource, ov *overrides) {
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
