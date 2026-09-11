// Package wiki is the "wiki" plugin: the second level of bough's
// two-level record. History (~/.bough/history) is the append-only log
// of everything that happened; the wiki (~/.bough/wiki) is markdown
// pages compiled from it by the llm-wiki skill, every claim citing the
// history entry it came from as `<session>#<seq>`. Nothing here reaches
// a prompt: an agent reads the wiki on demand, like any other file.
//
// The row mounts nothing. It contributes `bough wiki`: list the
// sessions not yet ingested, digest one for the ingest agent, check
// every citation and link, run one ingest (what the scheduler calls),
// and install that scheduler under launchd.
package wiki

import (
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// skillMD is the llm-wiki skill: the ingest, query and lint procedure.
// It ships in the binary and is written to ~/.bough/skills on install
// and on every run, so the procedure and the tooling never drift apart.
//
//go:embed SKILL.md
var skillMD string

const (
	defaultQuiet = 30 * time.Minute // a session untouched this long is finished enough to ingest
	defaultEvery = 5 * time.Minute  // how often the scheduler checks
	defaultMax   = 3                // sessions per ingest run; a backlog drains over several runs
	runTimeout   = 30 * time.Minute // one headless ingest, at most
)

func init() {
	kernel.Register("wiki", func() kernel.Plugin { return plugin{} })
}

type plugin struct{}

func (plugin) Name() string                                { return "wiki" }
func (plugin) Inject() []string                            { return nil }
func (plugin) Apply(*kernel.Context, map[string]any) error { return nil }

const usage = "pending [--all] | digest <session> [--from N] | check | run [--all] [--max N] | install [--every 5m] | uninstall"

func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "wiki",
		Usage:   usage,
		Summary: "the LLM wiki compiled from history (~/.bough/wiki): pending sessions, digests, citation check, ingest runs, the 5-minute scheduler",
		Run:     runCLI,
	}}
}

// paths is where everything lives. Tests build their own.
type paths struct {
	hist  string // ~/.bough/history
	wiki  string // ~/.bough/wiki
	skill string // ~/.bough/skills/llm-wiki/SKILL.md
	home  string
}

func (p paths) log() string   { return filepath.Join(p.wiki, "log.md") }
func (p paths) index() string { return filepath.Join(p.wiki, "index.md") }

func defaultPaths() (paths, error) {
	h, err := os.UserHomeDir()
	if err != nil {
		return paths{}, err
	}
	b := filepath.Join(h, ".bough")
	return paths{
		hist:  filepath.Join(b, "history"),
		wiki:  filepath.Join(b, "wiki"),
		skill: filepath.Join(b, "skills", "llm-wiki", "SKILL.md"),
		home:  h,
	}, nil
}

func runCLI(_ map[string]any, args []string) error {
	p, err := defaultPaths()
	if err != nil {
		return err
	}
	if len(args) == 0 {
		return fmt.Errorf("usage: bough wiki %s", usage)
	}
	flags := parseFlags(args[1:])
	switch args[0] {
	case "pending":
		pend, err := FindPending(p, defaultQuiet, flags.all, time.Now())
		if err != nil {
			return err
		}
		for _, x := range pend {
			fmt.Printf("%s\tentries %d..%d\t%s\t%s\n", x.ID, x.From+1, x.To, x.Last.Format("2006-01-02 15:04"), x.Title)
		}
		return nil
	case "digest":
		if len(flags.rest) != 1 {
			return fmt.Errorf("usage: bough wiki digest <session> [--from N]")
		}
		out, err := DigestFile(p, flags.rest[0], flags.from)
		if err != nil {
			return err
		}
		fmt.Print(out)
		return nil
	case "check":
		probs, err := Check(p)
		if err != nil {
			return err
		}
		for _, pr := range probs {
			fmt.Println(pr)
		}
		if len(probs) > 0 {
			return fmt.Errorf("%d problem(s)", len(probs))
		}
		fmt.Println("wiki ok")
		return nil
	case "run":
		exe, err := selfExe()
		if err != nil {
			return err
		}
		return Run(p, exe, flags.all, flags.max, defaultQuiet)
	case "install":
		exe, err := selfExe()
		if err != nil {
			return err
		}
		return install(p, exe, flags.every)
	case "uninstall":
		return uninstall(p)
	}
	return fmt.Errorf("usage: bough wiki %s", usage)
}

type cliFlags struct {
	all   bool
	from  int64
	max   int
	every time.Duration
	rest  []string
}

func parseFlags(args []string) cliFlags {
	f := cliFlags{max: defaultMax, every: defaultEvery}
	for i := 0; i < len(args); i++ {
		next := func() string {
			if i+1 < len(args) {
				i++
				return args[i]
			}
			return ""
		}
		switch args[i] {
		case "--all":
			f.all = true
		case "--from":
			f.from, _ = strconv.ParseInt(next(), 10, 64)
		case "--max":
			if n, err := strconv.Atoi(next()); err == nil && n > 0 {
				f.max = n
			}
		case "--every":
			if d, err := time.ParseDuration(next()); err == nil && d >= time.Minute {
				f.every = d
			}
		default:
			f.rest = append(f.rest, args[i])
		}
	}
	return f
}

func selfExe() (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", err
	}
	if p, err := filepath.EvalSymlinks(exe); err == nil {
		exe = p
	}
	return exe, nil
}
