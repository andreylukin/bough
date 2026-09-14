package container

import (
	"slices"
	"testing"
)

func TestScriptArgv(t *testing.T) {
	for _, c := range []struct {
		text string
		want []string
	}{
		{"#!/bin/bash\nset -euo pipefail\n", []string{"bash", "s.sh"}},
		{"#!/usr/bin/env bash\n", []string{"bash", "s.sh"}},
		{"#!/usr/bin/env -S bash -e\n", []string{"bash", "-e", "s.sh"}},
		{"#!/bin/sh\n", []string{"sh", "s.sh"}},
		{"set -e\napt-get update\n", []string{"sh", "s.sh"}},
		{"", []string{"sh", "s.sh"}},
		{"#!\n", []string{"sh", "s.sh"}},
	} {
		if got := ScriptArgv([]byte(c.text), "s.sh"); !slices.Equal(got, c.want) {
			t.Errorf("ScriptArgv(%q) = %q, want %q", c.text, got, c.want)
		}
	}
}
