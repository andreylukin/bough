package cmux

import (
	"context"
	"errors"
	"os"
	"strings"
	"testing"
)

func TestDetect(t *testing.T) {
	env := func(m map[string]string) func(string) string { return func(k string) string { return m[k] } }
	noPath := func(string) (string, error) { return "", errors.New("not found") }
	noStat := func(string) (os.FileInfo, error) { return nil, os.ErrNotExist }
	if cli, _ := Detect(env(nil), noPath, noStat); cli != "" {
		t.Fatal("outside cmux: nothing")
	}
	if cli, ws := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), func(string) (string, error) { return "/usr/local/bin/cmux", nil }, noStat); cli != "/usr/local/bin/cmux" || ws != "workspace:2" {
		t.Fatalf("on PATH: %q %q", cli, ws)
	}
	if cli, _ := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), noPath, func(p string) (os.FileInfo, error) {
		if p == candidates[0] {
			return nil, nil
		}
		return nil, os.ErrNotExist
	}); cli != candidates[0] {
		t.Fatalf("bundled: %q", cli)
	}
	if cli, _ := Detect(env(map[string]string{"CMUX_WORKSPACE_ID": "workspace:2"}), noPath, noStat); cli != "" {
		t.Fatal("no CLI anywhere: nothing")
	}
}

func TestNameOncePerTitle(t *testing.T) {
	var calls [][]string
	n := &Namer{workspace: "workspace:2", prefix: "bough · ", run: func(_ context.Context, args ...string) error { calls = append(calls, args); return nil }}
	n.Name("Deploy the event log")
	n.Name("Deploy the event log")
	n.Name("  ")
	n.Name("Second title")
	if len(calls) != 2 {
		t.Fatalf("calls: %v", calls)
	}
	if strings.Join(calls[0], " ") != "rename-workspace --workspace workspace:2 -- bough · Deploy the event log" {
		t.Fatalf("first call: %v", calls[0])
	}
}
