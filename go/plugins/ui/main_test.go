package ui

import (
	"os"
	"testing"
)

// TestMain points the native clipboard writer at "no tool available"
// (what CI's Linux runners have) for the whole package. Unstubbed, every
// drag or copy a test made ran the real pbcopy — writing the developer's
// clipboard, and costing a process per copy (100 of them in one rapid
// property). Tests that stub it restore this default, not the real one.
func TestMain(m *testing.M) {
	writeClipboardNative = noClipboard
	os.Exit(m.Run())
}

func noClipboard(string) []string { return nil }

// stubClipboard replaces the native clipboard writer for one test (not
// parallel-safe: callers must not use t.Parallel).
func stubClipboard(t *testing.T, f func(string) []string) {
	prev := writeClipboardNative
	writeClipboardNative = f
	t.Cleanup(func() { writeClipboardNative = prev })
}
