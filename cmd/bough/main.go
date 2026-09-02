// bough launcher: load the config tree, mount plugins, block.
package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/llm"
	_ "github.com/andreylukin/bough/plugins/loop"
	_ "github.com/andreylukin/bough/plugins/tools"
	_ "github.com/andreylukin/bough/plugins/ui"
)

// setFlags collects repeatable --set id.key=value overrides.
type setFlags []string

func (s *setFlags) String() string     { return strings.Join(*s, ",") }
func (s *setFlags) Set(v string) error { *s = append(*s, v); return nil }

func main() {
	var (
		config   = flag.String("config", "bough.yml", "path to config tree")
		headless = flag.Bool("headless", false, "read input from stdin, no TUI")
		web      = flag.String("web", "", "serve the UI in a browser at this addr (e.g. localhost:7681)")
		sets     setFlags
	)
	flag.Var(&sets, "set", "override row config: id.key=value (repeatable)")
	flag.Parse()

	rows, err := kernel.LoadFile(*config)
	if err != nil {
		fatal(err)
	}
	if err := applyOverrides(rows, sets); err != nil {
		fatal(err)
	}

	mode := "tui"
	switch {
	case *headless:
		mode = "headless"
	case *web != "":
		mode = "web:" + *web
	}

	ctx := kernel.NewContext()
	ctx.Provide("ui-mode", mode)

	if err := ctx.Mount(rows); err != nil {
		fatal(err)
	}

	// Block until interrupted, then unmount (effects run LIFO).
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
	<-sig
	ctx.Unmount()
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

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "bough:", err)
	os.Exit(1)
}
