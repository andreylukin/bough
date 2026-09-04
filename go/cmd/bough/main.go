// bough launcher: load the config tree, mount plugins, block.
package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"runtime/debug"
	"strings"
	"sync"
	"syscall"
	"text/tabwriter"
	"time"

	"github.com/fsnotify/fsnotify"

	"github.com/andreylukin/bough"
	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/ask"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/commands"
	_ "github.com/andreylukin/bough/plugins/contextmd"
	_ "github.com/andreylukin/bough/plugins/cost"
	_ "github.com/andreylukin/bough/plugins/graph"
	_ "github.com/andreylukin/bough/plugins/history"
	_ "github.com/andreylukin/bough/plugins/hooks"
	_ "github.com/andreylukin/bough/plugins/initjs"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/mcp"
	_ "github.com/andreylukin/bough/plugins/memory"
	_ "github.com/andreylukin/bough/plugins/prompts"
	_ "github.com/andreylukin/bough/plugins/skills"
	_ "github.com/andreylukin/bough/plugins/theme"
	_ "github.com/andreylukin/bough/plugins/title"
	_ "github.com/andreylukin/bough/plugins/todo"
	_ "github.com/andreylukin/bough/plugins/tools"
	"github.com/andreylukin/bough/plugins/ui"
	_ "github.com/andreylukin/bough/plugins/workers"
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

// configSource is where the config tree loads from: a file path, or
// the embedded default when path is "" (no file — no hot reload).
type configSource struct {
	path string
}

func (s configSource) load() ([]kernel.Row, error) {
	if s.path == "" {
		return kernel.LoadBytes(bough.DefaultConfig, "embedded default config")
	}
	return kernel.LoadFile(s.path)
}

// resolveConfig picks the config source. An explicit --config is used
// verbatim (a missing file stays fatal at load). Otherwise: ./bough.yml
// if present, else ~/.bough/bough.yml, else the embedded default.
// (main notes a non-./bough.yml source on stderr in TUI mode.)
func resolveConfig(explicit bool, flagVal string) configSource {
	if explicit {
		return configSource{path: flagVal}
	}
	if _, err := os.Stat("bough.yml"); err == nil {
		return configSource{path: "bough.yml"}
	}
	if home, err := os.UserHomeDir(); err == nil {
		global := filepath.Join(home, ".bough", "bough.yml")
		if _, err := os.Stat(global); err == nil {
			return configSource{path: global}
		}
	}
	return configSource{}
}

// describe names the source for the "bough: using ..." note.
func (s configSource) describe() string {
	if s.path == "" {
		return "embedded default config"
	}
	return s.path
}

// commands are the subcommands `bough <name>` dispatches to.
var commands = map[string]bool{"rows": true, "sessions": true, "log": true, "update": true, "restart": true, "web": true}

// command splits argv into the subcommand (if any) and its args. A
// first arg that is neither a flag nor a known subcommand is an error
// — the launcher must not fall through into the TUI on a typo.
func command(args []string) (name string, rest []string, err error) {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		return "", args, nil
	}
	if !commands[args[0]] {
		if _, ok := kernel.FindCommand(args[0]); ok {
			return args[0], args[1:], nil
		}
		return "", nil, fmt.Errorf("unknown command: %s (try --help)", args[0])
	}
	return args[0], args[1:], nil
}

// runPluginCommand dispatches `bough <name> args` to the plugin that
// contributed it, handing over the config of the first row running
// that plugin (from the same config source the TUI would load).
func runPluginCommand(pc kernel.PluginCommand, args []string) {
	if len(args) > 0 && (args[0] == "-h" || args[0] == "--help" || args[0] == "-help") {
		fmt.Fprintf(os.Stderr, "usage: bough %s %s   %s\n", pc.Name, pc.Usage, pc.Summary)
		return
	}
	var cfg map[string]any
	if rows, err := resolveConfig(false, "").load(); err == nil {
		for _, r := range rows {
			if r.Plugin == pc.Plugin && !r.Disabled {
				cfg = r.Config
				break
			}
		}
	}
	if err := pc.Run(cfg, args); err != nil {
		fmt.Fprintf(os.Stderr, "bough %s: %v\n", pc.Name, err)
		os.Exit(1)
	}
}

// version is set by -ldflags "-X main.version=..."; otherwise the
// module's VCS revision from the build info, else "dev".
var version = ""

func versionString() string {
	if version != "" {
		return version
	}
	if bi, ok := debug.ReadBuildInfo(); ok {
		rev, dirty := "", false
		for _, kv := range bi.Settings {
			switch kv.Key {
			case "vcs.revision":
				rev = kv.Value
			case "vcs.modified":
				dirty = kv.Value == "true"
			}
		}
		if rev != "" {
			if len(rev) > 8 {
				rev = rev[:8]
			}
			if dirty {
				rev += "-dirty"
			}
			return rev
		}
	}
	return "dev"
}

func main() {
	loadEnvFile() // ~/.bough/env: API keys for launchd/fresh shells
	cmd, args, err := command(os.Args[1:])
	if err != nil {
		fmt.Fprintln(os.Stderr, "bough:", err)
		os.Exit(2)
	}
	switch cmd {
	case "log":
		runLog(args)
		return
	case "sessions":
		runSessions(args)
		return
	case "update":
		runUpdate(args)
		return
	case "restart":
		runRestart(args)
		return
	case "web":
		runWeb(args)
		return
	}
	if pc, ok := kernel.FindCommand(cmd); ok && cmd != "" {
		runPluginCommand(pc, args)
		return
	}
	rowsCmd := cmd == "rows"
	// -c/--continue and -r/--resume [id] take an optional value, which
	// the flag package can't express; pull them out first.
	contFlag, resumeFlag, resumeID, args := extractSessionFlags(args)
	var (
		config   = flag.String("config", "", "path to config tree (default ./bough.yml, else ~/.bough/bough.yml, else embedded)")
		headless = flag.Bool("headless", false, "read input from stdin, no TUI")
		web      = flag.String("web", "", "serve the UI in a browser at this addr (e.g. localhost:7681)")
		dump     = flag.Bool("dump-config", false, "mount the config tree, print the row state table, and exit")
		verbose  = flag.Bool("verbose", false, "print kernel/mcp/config diagnostics on stderr")
		showVer  = flag.Bool("version", false, "print the version and exit")
		sets     setFlags
	)
	flag.Var(&sets, "set", "override row config: id.key=value (repeatable)")
	flag.CommandLine.Usage = usage
	flag.CommandLine.Parse(args)
	if *showVer {
		v := "bough " + versionString()
		if n := staleNotice(resolveExe()); n != "" {
			v += " (stale: " + n + ")"
		}
		fmt.Println(v)
		return
	}
	if *verbose {
		kernel.Verbose = true
	}

	explicitConfig := false
	flag.CommandLine.Visit(func(f *flag.Flag) {
		if f.Name == "config" {
			explicitConfig = true
		}
	})
	src := resolveConfig(explicitConfig, *config)

	mode := "tui"
	switch {
	case *headless:
		mode = "headless"
	case *web != "":
		mode = "web:" + *web
	}
	// Name a non-./bough.yml source once, where a person is looking
	// (TUI) — or under --verbose, so scripts can check which file won.
	if src.path != "bough.yml" && ((mode == "tui" && !rowsCmd && !*dump) || kernel.Verbose) {
		fmt.Fprintf(os.Stderr, "bough: using %s\n", src.describe())
	}

	// Resolve session flags before anything mounts: --continue and
	// --resume <id> become a history row override; a bare --resume in
	// tui/web asks for the picker (headless prints the list, exit 2).
	resumePath, needPicker := resolveSession(contFlag, resumeFlag, resumeID, mode)
	if resumePath != "" {
		sets = append(sets, "history.file="+resumePath)
	}

	rows, err := src.load()
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
	// A dev install running a build older than its checkout: say so
	// where a person will see it (the ui shows the "notice" service as
	// its first row; headless prints it), naming `bough update`.
	if n := staleNotice(resolveExe()); n != "" {
		if mode == "headless" {
			fmt.Fprintln(os.Stderr, "bough: "+n)
		} else {
			ctx.Provide("notice", n)
		}
	}

	ov := &overrides{vals: sets}
	// Runtime override seam: plugins (e.g. /model) change a row's config
	// or plugin at runtime through the same LoadFile + overrides +
	// Reconcile path as a config hot reload; applied sets are recorded so
	// a later hot reload keeps them (same mechanics as session resume).
	ctx.Provide("config-set", func(newSets ...string) error {
		return runtimeSet(ctx, src, ov, newSets...)
	})
	provideChoose(ctx, src, ov)
	if needPicker {
		providePicker(ctx)
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

	stopWatch, err := watchConfig(ctx, src, ov)
	if err != nil {
		fatal(err)
	}

	// Block until interrupted, then unmount (effects run LIFO). Exit 1
	// when a headless turn errored, else 0 (TUI /quit and ctrl+c too).
	<-sig
	stopWatch()
	ctx.Unmount()
	os.Exit(ui.ExitCode())
}

// watchConfig hot-reloads the config: fsnotify on the file's parent
// dir (editors replace files, so watching the file itself breaks),
// 300ms debounce, then parse + overrides + Reconcile. One-line result
// log either way; a bad candidate keeps the last good tree.
func watchConfig(ctx *kernel.Context, src configSource, ov *overrides) (func(), error) {
	if src.path == "" {
		kernel.Logf("bough: embedded config has no file; hot reload disabled\n")
		return func() {}, nil
	}
	w, err := fsnotify.NewWatcher()
	if err != nil {
		return nil, fmt.Errorf("watch config: %w", err)
	}
	abs, err := filepath.Abs(src.path)
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
				reload(ctx, src, ov.all())
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

// runtimeSet applies "id.key=value" overrides at runtime: fresh config
// parse + the recorded overrides + the new sets, then Reconcile — the
// same live-swap path the session picker uses. The new sets are
// recorded only on success, so a later config hot reload (which
// replays ov.all()) keeps them.
func runtimeSet(ctx *kernel.Context, src configSource, ov *overrides, sets ...string) error {
	rows, err := src.load()
	if err != nil {
		return err
	}
	if err := applyOverrides(rows, append(ov.all(), sets...)); err != nil {
		return err
	}
	if err := ctx.Reconcile(rows); err != nil {
		return err
	}
	for _, s := range sets {
		ov.add(s)
	}
	return nil
}

func reload(ctx *kernel.Context, src configSource, sets setFlags) {
	rows, err := src.load()
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
	fmt.Fprintf(os.Stderr, "bough: reloaded %s\n", src.path)
}

// applyOverrides applies each "id.key=value" to the matching row's config.
func applyOverrides(rows []kernel.Row, sets setFlags) error {
	for _, s := range sets {
		before, after, ok := strings.Cut(s, "=")
		if !ok {
			return fmt.Errorf("bad --set %q: want id.key=value", s)
		}
		path, value := before, after
		before0, after0, ok0 := strings.Cut(path, ".")
		if !ok0 {
			return fmt.Errorf("bad --set %q: want id.key=value", s)
		}
		id, key := before0, after0
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

const usageText = `usage: bough [flags]                 start the TUI
       bough <command> [args]

flags:
  -c, --continue          resume the most recent session
  -r, --resume [id]       resume a session by id, or pick from a list
      --set id.key=value  override a row's config (repeatable);
                          id.plugin=name swaps a row's plugin
      --config <path>     config tree (default ./bough.yml, else
                          ~/.bough/bough.yml, else the embedded default)
      --headless          read lines from stdin, print "[kind] text"
                          events on stdout ("[error]" on stderr; exit 1
                          if any turn errored), no TUI
      --web <addr>        serve the UI in a browser (e.g. localhost:7681)
      --dump-config       mount the config tree, print the row table, exit
      --verbose           kernel/mcp/config diagnostics on stderr
                          (also BOUGH_VERBOSE=1)
      --version           print the version and exit
  -h, --help              this help

Single-dash long flags (-set, -headless) are accepted too.

commands:
  rows      print the row state table and exit
  sessions  list stored sessions, newest first
  log       pretty-print a session's history (latest when no arg)
  update    git pull + rebuild this binary + restart the web session
  restart   bounce the running --web session onto the current binary
  web       [addr] start the browser UI detached and open it (default
            localhost:7681); "web status" / "web stop"

config:
  ./bough.yml           project config tree (row list)
  ~/.bough/bough.yml    global config, used when there is no ./bough.yml
  ~/.bough/init.js      startup script: providers, tools, settings
  ~/.bough/history/     one JSONL per session (bough sessions / log)
`

func usage() {
	fmt.Fprint(os.Stderr, usageText)
	if cmds := kernel.Commands(); len(cmds) > 0 {
		fmt.Fprintln(os.Stderr, "\nplugin commands:")
		for _, c := range cmds {
			left := c.Name
			if c.Usage != "" {
				left += " " + c.Usage
			}
			fmt.Fprintf(os.Stderr, "  %-34s %s\n", left, c.Summary)
		}
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "bough:", err)
	os.Exit(1)
}
