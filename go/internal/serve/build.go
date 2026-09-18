package serve

import (
	"fmt"
	"os"
	"runtime/debug"
	"sync"
)

// The control room embeds its UI in the binary, so `bough update`
// restarts the supervisor to get a new bundle served. That fixes the
// server and not the page: a tab opened before the update keeps running
// the JS it loaded with, and the fix looks like it did not land.
//
// So every API response carries the build that answered it. The page
// remembers the first one it saw and says so when it changes, which is
// the only moment a reload is worth asking for.

const BuildHeader = "X-Bough-Build"

// buildID names this binary: its VCS revision and dirty flag, plus the
// executable's path and mtime, so two builds of one dirty tree — the
// normal case in a checkout — still differ.
var buildID = sync.OnceValue(func() string {
	id := ""
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, kv := range bi.Settings {
			if kv.Key == "vcs.revision" || kv.Key == "vcs.modified" {
				id += kv.Value + " "
			}
		}
	}
	if exe, err := os.Executable(); err == nil {
		if fi, err := os.Stat(exe); err == nil {
			id += fmt.Sprintf("%s@%d", exe, fi.ModTime().UnixNano())
		}
	}
	if id == "" {
		id = "unknown"
	}
	return id
})
