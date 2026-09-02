// bough log [path]: pretty-print a history JSONL (latest session file
// when no arg). --raw prints the full JSON lines.
package main

import (
	"bufio"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

func runLog(args []string) {
	fs := flag.NewFlagSet("log", flag.ExitOnError)
	fs.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: bough log [--raw] [session-id|path]   (latest non-empty session when no arg)")
	}
	raw := fs.Bool("raw", false, "print full JSON lines")
	fs.Parse(args)
	if fs.NArg() > 1 {
		fs.Usage()
		os.Exit(2)
	}

	path := fs.Arg(0)
	if path == "" {
		p, err := latestSession(sessionsDir())
		if err != nil {
			fatal(err)
		}
		path = p
	} else if _, err := os.Stat(path); err != nil {
		// A session id (as `bough sessions` prints) works too.
		if p := filepath.Join(sessionsDir(), strings.TrimSuffix(path, ".jsonl")+".jsonl"); fileExists(p) {
			path = p
		}
	}
	f, err := os.Open(path)
	if err != nil {
		fatal(err)
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for sc.Scan() {
		if *raw {
			fmt.Println(sc.Text())
			continue
		}
		var e history.Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			fatal(fmt.Errorf("%s: %w", path, err))
		}
		text, _ := e.Data["text"].(string)
		preview := text
		if i := strings.IndexByte(preview, '\n'); i >= 0 {
			preview = preview[:i] + " …"
		}
		if len(preview) > 100 {
			preview = preview[:100] + "…"
		}
		fmt.Printf("%4d  %s  %-9s  %s\n", e.Seq, e.At.Format("15:04:05.000"), e.Kind, preview)
	}
	if err := sc.Err(); err != nil {
		fatal(err)
	}
}

// latestSession returns the newest session file in dir that has at
// least one entry: `bough rows` and an aborted launch each leave an
// empty file behind, and "latest" must never be one of those.
func latestSession(dir string) (string, error) {
	infos, err := history.List(dir)
	if err != nil {
		return "", err
	}
	for _, in := range infos {
		if in.Entries > 0 {
			return in.Path, nil
		}
	}
	return "", fmt.Errorf("no session files in %s", dir)
}

func fileExists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}
