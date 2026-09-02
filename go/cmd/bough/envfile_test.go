package main

import (
	"strings"
	"testing"
)

func TestApplyEnv(t *testing.T) {
	have := map[string]string{"KEEP": "orig"}
	applyEnv(strings.NewReader("# comment\nexport A=1\nB=\"two words\"\nKEEP=new\nbad line\nC='x'\n"),
		func(k string) string { return have[k] },
		func(k, v string) error { have[k] = v; return nil })
	if have["A"] != "1" || have["B"] != "two words" || have["C"] != "x" || have["KEEP"] != "orig" {
		t.Fatalf("env = %v", have)
	}
}
