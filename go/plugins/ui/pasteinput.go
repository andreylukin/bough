package ui

import (
	"bytes"
	"os"
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
		if s, e := bytes.LastIndex(chunk, pasteStart), bytes.LastIndex(chunk, pasteEnd); s > e {
			p.inPaste = true
		} else if e > s {
			p.inPaste = false
		}
		if p.inPaste {
			for k := min(len(pasteEnd)-1, n); k > 0; k-- {
				if bytes.HasSuffix(chunk, pasteEnd[:k]) {
					p.held = append(append([]byte(nil), chunk[n-k:]...), p.held...)
					n -= k
					break
				}
			}
		}
		if n > 0 {
			return n, nil
		}
	}
}
