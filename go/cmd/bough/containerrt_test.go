package main

import (
	"bytes"
	"errors"
	"strings"
	"testing"
)

type fakeCR struct {
	paths map[string]string // look name -> path; absent = missing
	fail  map[string]bool   // joined command -> fails
	calls []string
}

func (f *fakeCR) look(name string) (string, error) {
	if p, ok := f.paths[name]; ok {
		return p, nil
	}
	return "", errors.New("not found")
}

func (f *fakeCR) run(name string, args ...string) error {
	c := strings.Join(append([]string{name}, args...), " ")
	f.calls = append(f.calls, c)
	if f.fail[c] {
		return errors.New("exit 1")
	}
	if c == "brew install container" {
		f.paths["container"] = homebrewContainer
	}
	return nil
}

func TestEnsureContainerRuntime(t *testing.T) {
	t.Parallel()
	const bin = "/usr/local/bin/container"
	cases := []struct {
		name      string
		goos      string
		paths     map[string]string
		fail      []string
		wantCalls []string
		wantOut   string
	}{
		{name: "non-darwin no-op", goos: "linux", paths: map[string]string{}},
		{name: "running", goos: "darwin", paths: map[string]string{"container": bin},
			wantCalls: []string{bin + " system status"}},
		{name: "homebrew fallback path", goos: "darwin", paths: map[string]string{homebrewContainer: homebrewContainer},
			wantCalls: []string{homebrewContainer + " system status"}},
		{name: "no brew", goos: "darwin", paths: map[string]string{},
			wantOut: "bough: container runtime: Apple container CLI missing and Homebrew not found (install Homebrew then `brew install container`)"},
		{name: "install then start", goos: "darwin", paths: map[string]string{"brew": "/opt/homebrew/bin/brew"},
			fail:      []string{homebrewContainer + " system status"},
			wantCalls: []string{"brew install container", homebrewContainer + " system status", homebrewContainer + " system start --enable-kernel-install"}},
		{name: "brew fails warns", goos: "darwin", paths: map[string]string{"brew": "b"},
			fail:      []string{"brew install container"},
			wantCalls: []string{"brew install container"},
			wantOut:   "bough: container runtime: brew install container failed: exit 1 (run `brew install container`)"},
		{name: "flag rejected retries plain start", goos: "darwin", paths: map[string]string{"container": bin},
			fail:      []string{bin + " system status", bin + " system start --enable-kernel-install"},
			wantCalls: []string{bin + " system status", bin + " system start --enable-kernel-install", bin + " system start"}},
		{name: "start fails warns", goos: "darwin", paths: map[string]string{"container": bin},
			fail:      []string{bin + " system status", bin + " system start --enable-kernel-install", bin + " system start"},
			wantCalls: []string{bin + " system status", bin + " system start --enable-kernel-install", bin + " system start"},
			wantOut:   "bough: container runtime: container system start failed: exit 1 (run `container system start`)"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			f := &fakeCR{paths: tc.paths, fail: map[string]bool{}}
			for _, c := range tc.fail {
				f.fail[c] = true
			}
			var out bytes.Buffer
			ensureContainerRuntime(&out, f.run, f.look, tc.goos)
			if strings.Join(f.calls, "|") != strings.Join(tc.wantCalls, "|") {
				t.Fatalf("calls = %q, want %q", f.calls, tc.wantCalls)
			}
			if tc.wantOut != "" && !strings.Contains(out.String(), tc.wantOut) {
				t.Fatalf("out = %q, want %q", out.String(), tc.wantOut)
			}
			if tc.goos != "darwin" && out.Len() != 0 {
				t.Fatalf("non-darwin wrote %q", out.String())
			}
		})
	}
}
