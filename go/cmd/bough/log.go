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
	"sort"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

func runLog(args []string) {
	fs := flag.NewFlagSet("log", flag.ExitOnError)
	raw := fs.Bool("raw", false, "print full JSON lines")
	fs.Parse(args)

	path := fs.Arg(0)
	if path == "" {
		p, err := latestSession()
		if err != nil {
			fatal(err)
		}
		path = p
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

// latestSession returns the lexically last file in ~/.bough/history
// (RFC3339-prefixed names sort chronologically).
func latestSession() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(home, ".bough", "history")
	names, err := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	if err != nil || len(names) == 0 {
		return "", fmt.Errorf("no session files in %s", dir)
	}
	sort.Strings(names)
	return names[len(names)-1], nil
}
