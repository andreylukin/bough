package ui

import (
	"bytes"
	"os"
	"time"
)

var (
	pasteStart = []byte("\x1b[200~")
	pasteEnd   = []byte("\x1b[201~")
)

// pasteInput wraps the terminal's stdin so a paste-end sequence split
// across two reads reaches bubbletea whole. ultraviolet's scanner gives
// an incomplete sequence only its 50ms escape timeout; inside a paste it
// then drops "\x1b[20" and treats the trailing "1~" as paste text, so
// the paste never ends (a slow link or ssh splits reads like this).
// While a paste is open, a read ending in a prefix of the paste-end
// sequence keeps that tail back until the next read completes it.
type pasteInput struct {
	*os.File
	held    []byte // withheld tail, prepended to the next read
	inPaste bool
}

func (p *pasteInput) Read(b []byte) (int, error) {
	for {
		n := copy(b, p.held)
		p.held = p.held[n:]
		if n < len(b) {
			m, err := p.File.Read(b[n:])
			n += m
			if err != nil {
				return n, err
			}
		}
		chunk := b[:n]
		if !p.openAfter(chunk) {
			var err error
			n, err = p.waitStart(b, n)
			chunk = b[:n]
			if err != nil {
				return n, err
			}
		}
		p.inPaste = p.openAfter(chunk)
		if p.inPaste {
			if k := tailPrefix(chunk, pasteEnd); k > 0 {
				p.held = append(append([]byte(nil), chunk[n-k:]...), p.held...)
				n -= k
			}
		}
		if n > 0 {
			return n, nil
		}
	}
}

const (
	// startWait bounds how long a split paste-start tail waits for the rest.
	startWait = 100 * time.Millisecond
	// escTimeout is ultraviolet's DefaultEscTimeout: how long it holds an
	// incomplete sequence before flushing it as keys.
	escTimeout = 50 * time.Millisecond
)

// waitStart: outside a paste, b[:n] ending in a proper prefix of the
// paste-start sequence (a bare ESC included) waits up to startWait for
// the rest, gluing reads while they keep spelling it. Otherwise
// ultraviolet flushes the tail as keys at escTimeout and the paste body
// arrives as keystrokes (its newline submits). Nothing is held across
// reads. A bare ESC that sat alone past escTimeout (the rest never came,
// or something else did) is what ultraviolet would have flushed as the
// Escape key, so it goes on as CSI 27 u and is not glued to what follows
// as alt+key.
func (p *pasteInput) waitStart(b []byte, n int) (int, error) {
	k := tailPrefix(b[:n], pasteStart)
	if k == 0 {
		return n, nil
	}
	at, t0, lone := n-k, time.Now(), k == 1
	for n < len(b) {
		left := startWait - time.Since(t0)
		if left <= 0 || !waitReadable(p.Fd(), left) {
			break
		}
		if n-at == 1 && time.Since(t0) < escTimeout {
			lone = false // ultraviolet would still have been waiting too
		}
		m, err := p.File.Read(b[n:])
		n += m
		if err != nil {
			return n, err
		}
		if bytes.HasPrefix(b[at:n], pasteStart) {
			return n, nil
		}
		if !bytes.HasPrefix(pasteStart, b[at:n]) {
			break
		}
	}
	// "\x1b[201" after a lone ESC is a paste end ultraviolet is waiting
	// for (it may have opened a paste this reader missed): pass it whole.
	if !lone || n+4 > len(b) || n-at >= 5 && bytes.HasPrefix(b[at:n], pasteEnd[:5]) {
		return n, nil
	}
	copy(b[at+5:], b[at+1:n])
	copy(b[at+1:], "[27u")
	return n + 4, nil
}

// openAfter reports whether a paste is open once chunk has been read.
func (p *pasteInput) openAfter(chunk []byte) bool {
	if s, e := bytes.LastIndex(chunk, pasteStart), bytes.LastIndex(chunk, pasteEnd); s != e {
		return s > e
	}
	return p.inPaste
}

// tailPrefix is the length of the longest proper prefix of seq that
// chunk ends with (0 when none).
func tailPrefix(chunk, seq []byte) int {
	for k := min(len(seq)-1, len(chunk)); k > 0; k-- {
		if bytes.HasSuffix(chunk, seq[:k]) {
			return k
		}
	}
	return 0
}
