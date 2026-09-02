package main

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"
)

// loadEnvFile reads ~/.bough/env (KEY=value lines, # comments, optional
// export prefix and quotes) into the process environment, never
// overriding a variable that is already set. This is where API keys
// live on a machine that runs bough from launchd or a fresh shell.
func loadEnvFile() {
	home, err := os.UserHomeDir()
	if err != nil {
		return
	}
	f, err := os.Open(filepath.Join(home, ".bough", "env"))
	if err != nil {
		return
	}
	defer f.Close()
	applyEnv(f, os.Getenv, os.Setenv)
}

func applyEnv(r interface{ Read([]byte) (int, error) }, get func(string) string, set func(string, string) error) {
	sc := bufio.NewScanner(r)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		line = strings.TrimPrefix(line, "export ")
		k, v, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		k = strings.TrimSpace(k)
		v = strings.TrimSpace(v)
		if len(v) >= 2 && (v[0] == '"' || v[0] == '\'') && v[len(v)-1] == v[0] {
			v = v[1 : len(v)-1]
		}
		if k == "" || get(k) != "" {
			continue
		}
		_ = set(k, v)
	}
}
