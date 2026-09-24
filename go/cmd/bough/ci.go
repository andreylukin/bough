// `bough ci`: the checks of .bough/ci.yml against a checkpoint of the
// working tree, rerunning only those whose inputs changed. The logic is
// internal/ci; this is flags, output and exit codes. An agent calls it
// from its bash tool, so the report is short and the exit code is the
// answer: 0 pass, 1 a check failed, 2 not settled (or a usage error).
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"text/tabwriter"
	"time"

	"github.com/andreylukin/bough/internal/ci"
)

func runCI(args []string) {
	home, err := os.UserHomeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough ci:", err)
		os.Exit(2)
	}
	cwd, err := os.Getwd()
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough ci:", err)
		os.Exit(2)
	}
	// ^C kills the running check's process group rather than leaving a
	// build behind in the CI worktree.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	code := ciMain(ctx, args, os.Stdout, os.Stderr, home, cwd)
	stop()
	os.Exit(code)
}

// stringList is a repeatable string flag.
type stringList []string

func (s *stringList) String() string     { return strings.Join(*s, ",") }
func (s *stringList) Set(v string) error { *s = append(*s, v); return nil }

const ciUsage = `usage: bough ci [--no-wait] [--check <name>]... [--tree <tree-ish>] [--rerun] [--json] [--dir <path>]
       bough ci log <check> [--tree <tree-ish>] [--dir <path>]

Runs the checks in .bough/ci.yml (read from the tree being checked)
against a snapshot of the working tree, in a separate worktree under
~/.bough/ci/. A check whose inputs are unchanged is reported from its
stored result. Exit 0: all passed; 1: a check failed; 2: not settled.
`

func ciMain(ctx context.Context, args []string, stdout, stderr io.Writer, home, cwd string) int {
	if len(args) > 0 && args[0] == "log" {
		return ciLog(ctx, args[1:], stdout, stderr, home, cwd)
	}
	fs := flag.NewFlagSet("ci", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.Usage = func() { fmt.Fprint(stderr, ciUsage); fs.PrintDefaults() }
	var checks stringList
	fs.Var(&checks, "check", "run only this check, manual ones included (repeatable)")
	noWait := fs.Bool("no-wait", false, "report stored results only; never run a check")
	tree := fs.String("tree", "", "tree-ish to check (a commit, refs/bough/turns/<sid>/<seq>, a tree id); default: the working tree now")
	rerun := fs.Bool("rerun", false, "ignore stored results for the selected checks")
	asJSON := fs.Bool("json", false, "print the report as JSON")
	dir := fs.String("dir", cwd, "a directory inside the checkout")
	if err := fs.Parse(args); err != nil {
		if err == flag.ErrHelp {
			return 0
		}
		return 2
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(stderr, "bough ci: unexpected argument %q\n", fs.Arg(0))
		fs.Usage()
		return 2
	}
	if *noWait && *rerun {
		fmt.Fprintln(stderr, "bough ci: --no-wait and --rerun contradict each other")
		return 2
	}
	rep, err := ci.Run(ctx, ci.Options{
		Home: home, Dir: *dir, Tree: *tree, Checks: checks,
		NoWait: *noWait, Rerun: *rerun, Progress: stderr,
	})
	if err != nil {
		fmt.Fprintln(stderr, "bough", err)
		return 2
	}
	if *asJSON {
		enc := json.NewEncoder(stdout)
		enc.SetIndent("", "  ")
		enc.Encode(rep)
	} else {
		printCIReport(stdout, rep, home, time.Now())
	}
	return rep.ExitCode()
}

// printCIReport is one line per check, then where the answer is from.
func printCIReport(w io.Writer, rep ci.Report, home string, now time.Time) {
	tw := tabwriter.NewWriter(w, 2, 8, 2, ' ', 0)
	var failed []string
	for _, c := range rep.Checks {
		dur, from := "", ""
		if c.Result != nil {
			dur = fmtDuration(time.Duration(c.Result.DurationMS) * time.Millisecond)
			if c.Cached {
				from = "cached from " + shortTree(c.Result.Tree) + ", " + ago(now.Sub(c.Result.Finished))
			}
		}
		switch c.State {
		case ci.StateFail:
			failed = append(failed, c.Name)
		case ci.StateManual:
			from = "runs with --check " + c.Name
		case ci.StateRunning:
			from = "another bough ci holds the lock"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", c.Name, c.State, dur, from)
	}
	tw.Flush()
	fmt.Fprintf(w, "tree %s in %s (checks from %s)\n", shortTree(rep.Tree), tildePath(home, rep.Repo), rep.ConfigFrom)
	for _, n := range failed {
		fmt.Fprintf(w, "log: bough ci log %s\n", n)
	}
}

func shortTree(t string) string {
	if len(t) > 10 {
		return t[:10]
	}
	return t
}

func fmtDuration(d time.Duration) string {
	if d < time.Second {
		return d.Round(time.Millisecond).String()
	}
	return d.Round(100 * time.Millisecond).String()
}

func ago(d time.Duration) string {
	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	}
	return fmt.Sprintf("%dd ago", int(d.Hours()/24))
}

// ciLog copies a check's stored log for the tree to stdout. The check
// name may come before or after the flags.
func ciLog(ctx context.Context, args []string, stdout, stderr io.Writer, home, cwd string) int {
	fs := flag.NewFlagSet("ci log", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.Usage = func() { fmt.Fprint(stderr, ciUsage); fs.PrintDefaults() }
	tree := fs.String("tree", "", "tree-ish whose result to show; default: the working tree now")
	dir := fs.String("dir", cwd, "a directory inside the checkout")
	var pos []string
	for {
		if err := fs.Parse(args); err != nil {
			if err == flag.ErrHelp {
				return 0
			}
			return 2
		}
		if fs.NArg() == 0 {
			break
		}
		pos = append(pos, fs.Arg(0))
		args = fs.Args()[1:]
	}
	if len(pos) != 1 {
		fmt.Fprintln(stderr, "usage: bough ci log <check> [--tree <tree-ish>] [--dir <path>]")
		return 2
	}
	path, res, err := ci.Log(ctx, ci.Options{Home: home, Dir: *dir, Tree: *tree}, pos[0])
	if err != nil {
		fmt.Fprintln(stderr, "bough", err)
		return 2
	}
	f, err := os.Open(path)
	if err != nil {
		fmt.Fprintf(stderr, "bough ci: log of %s: %v\n", pos[0], err)
		return 2
	}
	defer f.Close()
	io.Copy(stdout, f)
	fmt.Fprintf(stderr, "ci: %s %s (exit %d) on tree %s\n", res.Check, res.Status, res.ExitCode, shortTree(res.Tree))
	return 0
}
