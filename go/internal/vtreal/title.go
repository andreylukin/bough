package vtreal

import (
	"bytes"
	"io"
	"sync"
)

// titleFilter lifts OSC 0/2 (window title) out of the byte stream
// before it reaches x/vt. The x/ansi parser reads a raw 0x9c byte
// inside an OSC as ST, so a title with a UTF-8 glyph whose encoding
// contains it ("✓" is e2 9c 93) ends the sequence after its first
// byte and the rest of the title prints on screen as text. Real
// terminals decode UTF-8 first and never see it; the filter keeps the
// emulator honest and records the title itself.
type titleFilter struct {
	w     io.Writer
	title func(string)

	mu   sync.Mutex
	esc  bool   // a lone ESC was the last byte written
	in   bool   // inside an OSC we are capturing
	stEs bool   // ESC seen inside the OSC (ESC \ is ST)
	buf  []byte // the OSC payload so far
}

func newTitleFilter(w io.Writer, title func(string)) *titleFilter {
	return &titleFilter{w: w, title: title}
}

func (f *titleFilter) Write(p []byte) (int, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []byte
	for i := 0; i < len(p); i++ {
		b := p[i]
		switch {
		case f.in:
			if f.stEs {
				f.stEs = false
				if b == '\\' {
					out = f.end(out)
					continue
				}
				f.buf = append(f.buf, 0x1b)
			}
			switch b {
			case 0x07:
				out = f.end(out)
			case 0x1b:
				f.stEs = true
			default:
				f.buf = append(f.buf, b)
			}
		case f.esc:
			f.esc = false
			if b == ']' {
				f.in = true
				f.buf = f.buf[:0]
				continue
			}
			out = append(out, 0x1b, b)
		case b == 0x1b:
			f.esc = true
		default:
			out = append(out, b)
		}
	}
	if len(out) > 0 {
		if _, err := f.w.Write(out); err != nil {
			return 0, err
		}
	}
	return len(p), nil
}

// end dispatches a finished OSC: titles are recorded and dropped,
// anything else (hyperlinks, colour queries, clipboard) goes back into
// the stream in place — appended to out, never written ahead of it,
// or the bytes before it in the same chunk would arrive after it.
func (f *titleFilter) end(out []byte) []byte {
	f.in = false
	payload := f.buf
	f.buf = nil
	if cmd, rest, ok := bytes.Cut(payload, []byte(";")); ok && (string(cmd) == "0" || string(cmd) == "2") {
		f.title(string(rest))
		return out
	}
	out = append(out, 0x1b, ']')
	out = append(out, payload...)
	return append(out, 0x07)
}
