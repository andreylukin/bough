// `bough search`: find a stored session by what was said in it.
//
// `bough sessions` lists by title, which is the first prompt — useful
// only when you remember how a task opened. The thing you remember is
// usually something said in the middle of it, so this searches every
// transcript and reports the lines that matched. Results are grouped
// by when they were last active, because recency is how people reach
// for past work; the groups make the long tail scannable rather than
// hiding it behind a cap.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// defaultSearchLimit is what one screen can hold; --limit raises it.
const defaultSearchLimit = 20

func runSearch(args []string) {
	fs := flag.NewFlagSet("search", flag.ExitOnError)
	fs.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: bough search [--limit N] <query>")
		fmt.Fprintln(os.Stderr, "  words match anywhere in a transcript; repo:<name> branch:<name> since:<7d|24h|2w|YYYY-MM-DD> filter")
	}
	limit := fs.Int("limit", defaultSearchLimit, "most sessions to print (0 = no cap)")
	fs.Parse(args)
	if fs.NArg() == 0 {
		fs.Usage()
		os.Exit(2)
	}
	q, err := history.ParseQuery(strings.Join(fs.Args(), " "))
	if err != nil {
		fatal(err)
	}
	matches, err := history.Search(sessionsDir(), q, *limit)
	if err != nil {
		fatal(err)
	}
	if len(matches) == 0 {
		fmt.Fprintf(os.Stderr, "bough: nothing matched %q\n", strings.Join(fs.Args(), " "))
		return
	}
	printMatches(os.Stdout, matches, time.Now())
}

// printMatches renders the results under date headings, newest first.
func printMatches(w io.Writer, matches []history.Match, now time.Time) {
	home, _ := os.UserHomeDir()
	group := ""
	for _, m := range matches {
		if g := dateGroup(m.ModTime, now); g != group {
			group = g
			fmt.Fprintf(w, "\n%s\n", group)
		}
		tw := tabwriter.NewWriter(w, 2, 8, 2, ' ', 0)
		fmt.Fprintf(tw, "  %s\t%s\t%s\t%s\n",
			m.ID, m.ModTime.Local().Format("15:04"), place(m, home), truncate(m.Title, 60))
		tw.Flush()
		for _, l := range m.Lines {
			fmt.Fprintf(w, "      %s %s\n", l.Kind, truncate(l.Text, 96))
		}
		// Say what the preview left out, so a heavily matching session
		// reads as one result rather than looking arbitrarily trimmed.
		if extra := m.Hits - len(m.Lines); extra > 0 {
			fmt.Fprintf(w, "      … %d more in this session\n", extra)
		}
	}
}

// place is where the session worked: its branch on its repo when
// recorded, else the working directory, ~-abbreviated.
func place(m history.Match, home string) string {
	dir := m.Repo
	if dir == "" {
		dir = m.Cwd
	}
	if dir == "" {
		return "?"
	}
	if home != "" && strings.HasPrefix(dir, home+string(filepath.Separator)) {
		dir = "~" + strings.TrimPrefix(dir, home)
	}
	if m.Branch != "" {
		return dir + " " + m.Branch
	}
	return dir
}

// dateGroup is the heading a session falls under. The buckets come
// from how personal re-finding actually distributes: most of what you
// reach for is from today or this week, with a long tail that still
// has to be reachable.
func dateGroup(t, now time.Time) string {
	day := now.YearDay() - t.YearDay() + (now.Year()-t.Year())*366
	switch {
	case t.After(now):
		return "today"
	case day <= 0:
		return "today"
	case day == 1:
		return "yesterday"
	case day <= 7:
		return "last 7 days"
	case day <= 31:
		return "this month"
	}
	return "older"
}

// truncate caps s at n runes with an ellipsis, first line only.
func truncate(s string, n int) string {
	s, _, _ = strings.Cut(s, "\n")
	if r := []rune(s); len(r) > n {
		return string(r[:n-1]) + "…"
	}
	return s
}
