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
		// Outside a paste, a read ending in a prefix of the paste-start
		// sequence (a bare ESC included) waits briefly for the rest:
		// ultraviolet would flush it as keys at its 50ms timeout and the
		// paste body would arrive as keystrokes (its newline submits).
		// Not held across reads, so a real Escape key costs at most
		// startWait.
		for n < len(b) && !p.openAfter(chunk) && tailPrefix(chunk, pasteStart) > 0 && waitReadable(p.Fd(), startWait) {
			m, err := p.File.Read(b[n:])
			n += m
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

// startWait bounds how long a split paste-start tail waits for the rest.
const startWait = 100 * time.Millisecond

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
