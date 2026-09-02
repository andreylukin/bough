// bough launcher: load the config tree, mount plugins, block.
package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"text/tabwriter"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/commands"
	_ "github.com/andreylukin/bough/plugins/contextmd"
	_ "github.com/andreylukin/bough/plugins/history"
	_ "github.com/andreylukin/bough/plugins/hooks"
	_ "github.com/andreylukin/bough/plugins/initjs"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/mcp"
	_ "github.com/andreylukin/bough/plugins/skills"
	_ "github.com/andreylukin/bough/plugins/tools"
	_ "github.com/andreylukin/bough/plugins/ui"
)

// setFlags collects repeatable --set id.key=value overrides.
type setFlags []string

func (s *setFlags) String() string     { return strings.Join(*s, ",") }
func (s *setFlags) Set(v string) error { *s = append(*s, v); return nil }

// overrides is the shared --set list: hot reload re-applies it, and the
// session picker appends the resumed file's override at runtime, so a
// later config reload keeps the resumed session.
type overrides struct {
	mu   sync.Mutex
	vals setFlags
}

func (o *overrides) add(v string) {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.vals = append(o.vals, v)
}

func (o *overrides) all() setFlags {
	o.mu.Lock()
	defer o.mu.Unlock()
	return append(setFlags(nil), o.vals...)
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "log" {
		runLog(os.Args[2:])
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "sessions" {
		runSessions(os.Args[2:])
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "update" {
		runUpdate(os.Args[2:])
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "restart" {
		runRestart(os.Args[2:])
		return
	}
	rowsCmd := len(os.Args) > 1 && os.Args[1] == "rows"
	args := os.Args[1:]
	if rowsCmd {
		args = os.Args[2:]
	}
	// -c/--continue and -r/--resume [id] take an optional value, which
	// the flag package can't express; pull them out first.
	contFlag, resumeFlag, resumeID, args := extractSessionFlags(args)
	var (
		config   = flag.String("config", "bough.yml", "path to config tree")
		headless = flag.Bool("headless", false, "read input from stdin, no TUI")
		web      = flag.String("web", "", "serve the UI in a browser at this addr (e.g. localhost:7681)")
		dump     = flag.Bool("dump-config", false, "mount the config tree, print the row state table, and exit")
		sets     setFlags
	)
	flag.Var(&sets, "set", "override row config: id.key=value (repeatable)")
	flag.CommandLine.Usage = usage
	flag.CommandLine.Parse(args)

	mode := "tui"
	switch {
	case *headless:
		mode = "headless"
	case *web != "":
		mode = "web:" + *web
	}

	// Resolve session flags before anything mounts: --continue and
	// --resume <id> become a history row override; a bare --resume in
	// tui/web asks for the picker (headless prints the list, exit 2).
	resumePath, needPicker := resolveSession(contFlag, resumeFlag, resumeID, mode)
	if resumePath != "" {
		sets = append(sets, "history.file="+resumePath)
	}

	rows, err := kernel.LoadFile(*config)
	if err != nil {
		fatal(err)
	}
	if err := applyOverrides(rows, sets); err != nil {
		fatal(err)
	}

	// 'bough rows' and --dump-config: mount the tree fresh (tolerant —
	// Failed and Pending rows are the point of the table, not fatal),
	// print the live state table, and exit without starting a real UI.
	// Reconcile on an empty context is that tolerant mount.
	if rowsCmd || *dump {
		// The headless ui row interrupts the process on stdin EOF; this
		// command prints and exits on its own, so swallow that signal.
		signal.Notify(make(chan os.Signal, 1), os.Interrupt)
		ctx := kernel.NewContext()
		ctx.Provide("ui-mode", "headless")
		if err := ctx.Reconcile(rows); err != nil {
			fatal(err)
		}
		printRows(ctx)
		ctx.Unmount()
		return
	}

	// Catch interrupts BEFORE the mount: the headless ui row interrupts
	// the process on stdin EOF, which on a fast run can fire before main
	// reaches the wait below — with no handler installed yet the default
	// disposition would kill the process instead of unmounting cleanly.
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM)

	ctx := kernel.NewContext()
	ctx.Provide("ui-mode", mode)

	ov := &overrides{vals: sets}
	if needPicker {
		providePicker(ctx, rows, ov)
	}

	if err := ctx.Mount(rows); err != nil {
		fatal(err)
	}

	// A web session records "<pid> <addr>" so `bough restart` can find
	// it; the deferred remove runs after the clean unmount below.
	if *web != "" {
		if rm := writeWebPidfile(*web); rm != nil {
			defer rm()
		}
	}

	stopWatch, err := watchConfig(ctx, *config, ov)
	if err != nil {
		fatal(err)
	}

	// Block until interrupted, then unmount (effects run LIFO).
	<-sig
	stopWatch()
	ctx.Unmount()
}

// watchConfig hot-reloads the config: fsnotify on the file's parent
// dir (editors replace files, so watching the file itself breaks),
// 300ms debounce, then parse + overrides + Reconcile. One-line result
// log either way; a bad candidate keeps the last good tree.
func watchConfig(ctx *kernel.Context, config string, ov *overrides) (func(), error) {
	w, err := fsnotify.NewWatcher()
	if err != nil {
		return nil, fmt.Errorf("watch config: %w", err)
	}
	abs, err := filepath.Abs(config)
	if err != nil {
		return nil, fmt.Errorf("watch config: %w", err)
	}
	if err := w.Add(filepath.Dir(abs)); err != nil {
		return nil, fmt.Errorf("watch config: %w", err)
	}
	go func() {
		var pending <-chan time.Time
		for {
			select {
			case ev, ok := <-w.Events:
				if !ok {
					return
				}
				if filepath.Clean(ev.Name) != abs {
					continue
				}
				pending = time.After(300 * time.Millisecond)
			case <-pending:
				pending = nil
				reload(ctx, config, ov.all())
			case err, ok := <-w.Errors:
				if !ok {
					return
				}
				fmt.Fprintln(os.Stderr, "bough: config watch:", err)
			}
		}
	}()
	return func() { w.Close() }, nil
}

func reload(ctx *kernel.Context, config string, sets setFlags) {
	rows, err := kernel.LoadFile(config)
	if err != nil {
		fmt.Fprintf(os.Stderr, "bough: reload: %v (keeping current tree)\n", err)
		return
	}
	if err := applyOverrides(rows, sets); err != nil {
		fmt.Fprintf(os.Stderr, "bough: reload: %v (keeping current tree)\n", err)
		return
	}
	if err := ctx.Reconcile(rows); err != nil {
		fmt.Fprintf(os.Stderr, "bough: reload: %v\n", err)
		return
	}
	fmt.Fprintf(os.Stderr, "bough: reloaded %s\n", config)
}

// applyOverrides applies each "id.key=value" to the matching row's config.
func applyOverrides(rows []kernel.Row, sets setFlags) error {
	for _, s := range sets {
		eq := strings.IndexByte(s, '=')
		if eq < 0 {
			return fmt.Errorf("bad --set %q: want id.key=value", s)
		}
		path, value := s[:eq], s[eq+1:]
		dot := strings.IndexByte(path, '.')
		if dot < 0 {
			return fmt.Errorf("bad --set %q: want id.key=value", s)
		}
		id, key := path[:dot], path[dot+1:]
		found := false
		for i := range rows {
			if rows[i].ID == id {
				if key == "plugin" {
					rows[i].Plugin = value
				} else {
					if rows[i].Config == nil {
						rows[i].Config = map[string]any{}
					}
					rows[i].Config[key] = value
				}
				found = true
			}
		}
		if !found {
			return fmt.Errorf("--set %q: no row with id %q", s, id)
		}
	}
	return nil
}

// printRows renders the composed rows + live state table from Rows().
func printRows(ctx *kernel.Context) {
	w := tabwriter.NewWriter(os.Stdout, 2, 8, 2, ' ', 0)
	fmt.Fprintln(w, "ID\tPLUGIN\tSTATE\tDETAIL")
	for _, r := range ctx.Rows() {
		detail := ""
		switch r.State {
		case kernel.StatePending:
			detail = "missing: " + strings.Join(r.Missing, ", ")
		case kernel.StateFailed:
			detail = r.Err.Error()
		}
		fmt.Fprintf(w, "%s\t%s\t%s\t%s\n", r.ID, r.Plugin, r.State, detail)
	}
	w.Flush()
}

func usage() {
	fmt.Fprint(os.Stderr, `usage: bough [flags]
       bough <command> [args]

commands:
  rows      print the row state table and exit
  sessions  list stored sessions, newest first
  log       pretty-print a history JSONL (latest when no arg)
  update    git pull + rebuild this binary + restart the web session
  restart   bounce the running --web session onto the current binary

flags:
`)
	flag.PrintDefaults()
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "bough:", err)
	os.Exit(1)
}
