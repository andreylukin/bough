package container

import (
	"path/filepath"
	"strings"
)

// ScriptArgv is how to run a project script (setup.sh, resume.sh): with the
// interpreter its #! line names, else sh. Both scripts used to run as
// `sh script`, which ignores the shebang; on Debian sh is dash, so a
// `#!/bin/bash` script with `set -o pipefail` failed the image build and
// every later start of that project's sessions.
func ScriptArgv(text []byte, path string) []string {
	first, _, _ := strings.Cut(string(text), "\n")
	line, ok := strings.CutPrefix(strings.TrimSpace(first), "#!")
	if !ok {
		return []string{"sh", path}
	}
	fields := strings.Fields(line)
	if len(fields) > 0 && filepath.Base(fields[0]) == "env" {
		fields = fields[1:]
		for len(fields) > 0 && strings.HasPrefix(fields[0], "-") {
			fields = fields[1:] // env -S and friends
		}
	}
	if len(fields) == 0 {
		return []string{"sh", path}
	}
	// The interpreter by name, so /usr/bin/env-less images and a bash
	// outside /bin both resolve it through PATH; flags after it are kept.
	argv := append([]string{filepath.Base(fields[0])}, fields[1:]...)
	return append(argv, path)
}
