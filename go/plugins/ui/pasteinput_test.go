package ui

import (
	"os"
	"testing"
)

// A paste-end split across two writes comes out of pasteInput whole.
func TestPasteInputHoldsSplitEnd(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	defer w.Close()
	p := &pasteInput{File: r}
	buf := make([]byte, 64)
	w.WriteString("\x1b[200~split paste\x1b[20")
	n, _ := p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[200~split paste" {
		t.Fatalf("first read = %q", got)
	}
	w.WriteString("1~x")
	n, _ = p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[201~x" {
		t.Fatalf("second read = %q", got)
	}
	// Outside a paste nothing is held back.
	w.WriteString("\x1b[20")
	n, _ = p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[20" {
		t.Fatalf("outside paste = %q", got)
	}
}
