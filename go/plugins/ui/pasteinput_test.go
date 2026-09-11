package ui

import (
	"os"
	"testing"
	"time"
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

// A paste-start split across writes 50ms apart comes out of pasteInput
// whole, so the reader never flushes a bare ESC or "\x1b[2" as keys.
func TestPasteInputWaitsForSplitStart(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	defer w.Close()
	p := &pasteInput{File: r}
	buf := make([]byte, 64)
	for _, first := range []string{"\x1b", "\x1b[2", "\x1b[200"} {
		w.WriteString(first)
		go func() {
			time.Sleep(50 * time.Millisecond)
			w.WriteString("\x1b[200~"[len(first):] + "x")
		}()
		n, _ := p.Read(buf)
		if got := string(buf[:n]); got != "\x1b[200~x" {
			t.Fatalf("split %q read = %q", first, got)
		}
		w.WriteString("\x1b[201~")
		p.Read(buf)
	}
}

// A bare ESC left alone past ultraviolet's timeout is the Escape key: it
// goes on as CSI 27 u, never glued to a later key as alt+key.
func TestPasteInputLoneEscIsEscape(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	defer w.Close()
	p := &pasteInput{File: r}
	buf := make([]byte, 64)
	w.WriteString("\x1b")
	go func() {
		time.Sleep(70 * time.Millisecond)
		w.WriteString("/")
	}()
	n, _ := p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[27u/" {
		t.Fatalf("esc then late key = %q", got)
	}
	w.WriteString("\x1b")
	n, _ = p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[27u" {
		t.Fatalf("esc alone = %q", got)
	}
	// A late "[201~" is a paste end, never an Escape key.
	w.WriteString("\x1b")
	go func() {
		time.Sleep(70 * time.Millisecond)
		w.WriteString("[201~")
	}()
	n, _ = p.Read(buf)
	if got := string(buf[:n]); got != "\x1b[201~" {
		t.Fatalf("esc then late paste end = %q", got)
	}
}
