// Package ciflags is `bough ci`'s command line, shared by the command
// and the orb relay. It is a leaf so the relay can import it: internal/ci
// reaches internal/orb through plugins/history.
package ciflags

import (
	"flag"
	"strings"
)

// The flag sets are not in cmd/bough because the orb relay has
// to read a guest's argv exactly as the host binary will. It used to
// scan for --no-wait by hand and only ever turned it on, and flag lets
// the last occurrence win: `--no-wait --no-wait=false` passed the relay
// as read-only and then ran every check on the host.

// stringList is a repeatable string flag.
type stringList []string

func (s *stringList) String() string     { return strings.Join(*s, ",") }
func (s *stringList) Set(v string) error { *s = append(*s, v); return nil }

// RunFlags are `bough ci`'s.
type RunFlags struct {
	Checks stringList
	NoWait *bool
	Tree   *string
	Rerun  *bool
	JSON   *bool
	Dir    *string
}

// NewRunFlagSet defines `bough ci`'s flags; --dir defaults to cwd.
func NewRunFlagSet(cwd string) (*flag.FlagSet, *RunFlags) {
	fs := flag.NewFlagSet("ci", flag.ContinueOnError)
	f := &RunFlags{}
	fs.Var(&f.Checks, "check", "run only this check, manual ones included (repeatable)")
	f.NoWait = fs.Bool("no-wait", false, "report stored results only; never run a check")
	f.Tree = fs.String("tree", "", "tree-ish to check (a commit, refs/bough/turns/<sid>/<seq>, a tree id); default: the working tree now")
	f.Rerun = fs.Bool("rerun", false, "ignore stored results for the selected checks")
	f.JSON = fs.Bool("json", false, "print the report as JSON")
	f.Dir = fs.String("dir", cwd, "a directory inside the checkout")
	return fs, f
}

// LogFlags are `bough ci log`'s.
type LogFlags struct {
	Tree *string
	Dir  *string
}

// NewLogFlagSet defines `bough ci log`'s flags; --dir defaults to cwd.
func NewLogFlagSet(cwd string) (*flag.FlagSet, *LogFlags) {
	fs := flag.NewFlagSet("ci log", flag.ContinueOnError)
	f := &LogFlags{}
	f.Tree = fs.String("tree", "", "tree-ish whose result to show; default: the working tree now")
	f.Dir = fs.String("dir", cwd, "a directory inside the checkout")
	return fs, f
}

// ParseInterleaved parses args with positionals allowed between flags
// (`ci log vet --tree X` and `ci log --tree X vet`), returning them.
func ParseInterleaved(fs *flag.FlagSet, args []string) ([]string, error) {
	var pos []string
	for {
		if err := fs.Parse(args); err != nil {
			return nil, err
		}
		if fs.NArg() == 0 {
			return pos, nil
		}
		pos = append(pos, fs.Arg(0))
		args = fs.Args()[1:]
	}
}

// FlagSet reports whether name was given on the command line.
func FlagSet(fs *flag.FlagSet, name string) bool {
	set := false
	fs.Visit(func(f *flag.Flag) { set = set || f.Name == name })
	return set
}
