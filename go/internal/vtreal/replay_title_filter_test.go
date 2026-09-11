package vtreal

// Harness tests for title.go. The filter is a byte-stream rewriter, so
// the properties that matter are: the output is the input with OSC 0/2
// lifted out, the titles come back in order, and NOTHING depends on how
// the stream is cut into Write calls.
//
// The rapid run is opt-in (the default gate stays a fixed table):
//
//	BOUGH_FUZZ_TITLE=1 go test ./internal/vtreal -run TestTitleFilterProperty -rapid.checks=500

import (
	"bytes"
	"fmt"
	"os"
	"slices"
	"strings"
	"testing"

	"pgregory.net/rapid"
)

// titleRun feeds chunks through a fresh filter and returns what reached
// the writer and the titles it recorded.
func titleRun(chunks [][]byte) (string, []string, error) {
	var out bytes.Buffer
	var titles []string
	f := newTitleFilter(&out, func(s string) { titles = append(titles, s) })
	for _, c := range chunks {
		n, err := f.Write(c)
		if err != nil {
			return out.String(), titles, err
		}
		if n != len(c) {
			return out.String(), titles, fmt.Errorf("Write(%q) = %d, want %d", c, n, len(c))
		}
	}
	return out.String(), titles, nil
}

// titleChunk cuts b into pieces at the given sorted offsets.
func titleChunk(b []byte, cuts []int) [][]byte {
	var chunks [][]byte
	prev := 0
	for _, c := range cuts {
		if c <= prev || c > len(b) {
			continue
		}
		chunks = append(chunks, b[prev:c])
		prev = c
	}
	return append(chunks, b[prev:])
}

// titleWant is the specification: a second, independent scan of the
// stream that strips OSC 0/2 and hands every other OSC back (BEL
// terminated, as the filter re-emits it). Bytes of an unterminated
// sequence are still being parsed and have not been written yet.
func titleWant(b []byte) (string, []string) {
	var out, osc []byte
	var titles []string
	const (
		plain = iota
		afterESC
		inOSC
		inOSCAfterESC
	)
	state := plain
	flush := func() {
		cmd, rest, ok := bytes.Cut(osc, []byte(";"))
		if ok && (string(cmd) == "0" || string(cmd) == "2") {
			titles = append(titles, string(rest))
		} else {
			out = append(out, 0x1b, ']')
			out = append(out, osc...)
			out = append(out, 0x07)
		}
		osc = nil
		state = plain
	}
	for _, c := range b {
		switch state {
		case plain:
			if c == 0x1b {
				state = afterESC
			} else {
				out = append(out, c)
			}
		case afterESC:
			if c == ']' {
				state, osc = inOSC, nil
			} else {
				out = append(out, 0x1b, c)
				state = plain
			}
		case inOSCAfterESC:
			if c == '\\' {
				flush()
				continue
			}
			osc = append(osc, 0x1b)
			state = inOSC
			fallthrough
		case inOSC:
			switch c {
			case 0x07:
				flush()
			case 0x1b:
				state = inOSCAfterESC
			default:
				osc = append(osc, c)
			}
		}
	}
	return string(out), titles
}

func titleCheck(t *testing.T, name string, in []byte, chunks [][]byte) {
	t.Helper()
	wantOut, wantTitles := titleWant(in)
	gotOut, gotTitles, err := titleRun(chunks)
	if err != nil {
		t.Fatalf("%s: write: %v\ninput:  %q\nchunks: %q", name, err, in, chunks)
	}
	if gotOut != wantOut {
		t.Fatalf("%s: stream\ninput:  %q\nchunks: %q\ngot:    %q\nwant:   %q", name, in, chunks, gotOut, wantOut)
	}
	if strings.Join(gotTitles, "\x00") != strings.Join(wantTitles, "\x00") {
		t.Fatalf("%s: titles\ninput:  %q\nchunks: %q\ngot:    %q\nwant:   %q", name, in, chunks, gotTitles, wantTitles)
	}
}

var titleCases = []struct {
	name   string
	in     string
	out    string
	titles []string
}{
	{"plain text", "hello\r\nworld", "hello\r\nworld", nil},
	{"bel terminator", "a\x1b]2;hi\x07b", "ab", []string{"hi"}},
	{"st terminator", "a\x1b]0;hi\x1b\\b", "ab", []string{"hi"}},
	{"empty title", "\x1b]2;\x07x", "x", []string{""}},
	{"semicolons in title", "\x1b]2;a;b;c\x07", "", []string{"a;b;c"}},
	{"two titles in order", "\x1b]0;one\x07mid\x1b]2;two\x1b\\", "mid", []string{"one", "two"}},
	{"nested esc in payload", "\x1b]2;a\x1bXb\x07", "", []string{"a\x1bXb"}},
	{"esc esc then st", "\x1b]2;a\x1b\x1b\\", "", []string{"a\x1b"}},
	{"c1 st byte in utf8 payload", "\x1b]2;✓ ok\x07!", "!", []string{"✓ ok"}},
	{"raw c1 bytes in payload", "\x1b]2;\x9c\x9b\x85\x07", "", []string{"\x9c\x9b\x85"}},
	{"hyperlink forwarded", "A\x1b]8;;http://x\x07B", "A\x1b]8;;http://x\x07B", nil},
	{"colour query forwarded in place", "A\x1b]11;?\x07B", "A\x1b]11;?\x07B", nil},
	{"osc st forwarded as bel", "\x1b]11;?\x1b\\", "\x1b]11;?\x07", nil},
	{"osc without semicolon forwarded", "\x1b]777\x07", "\x1b]777\x07", nil},
	{"osc 20 is not a title", "\x1b]20;x\x07", "\x1b]20;x\x07", nil},
	{"csi untouched", "\x1b[1mF\x1b[0m", "\x1b[1mF\x1b[0m", nil},
	{"esc esc outside osc", "\x1b\x1bZ", "\x1b\x1bZ", nil},
	{"lone esc at end is held", "ab\x1b", "ab", nil},
	{"unterminated osc is held", "ab\x1b]2;par", "ab", nil},
	{"title around styled text", "\x1b]2;t\x07\x1b[1mF\x1b]0;u\x1b\\g", "\x1b[1mFg", []string{"t", "u"}},
}

// The table is the default gate: one Write per case, the plain reading
// of what the filter must do.
func TestTitleFilterCases(t *testing.T) {
	t.Parallel()
	for _, c := range titleCases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			in := []byte(c.in)
			gotOut, gotTitles, err := titleRun([][]byte{in})
			if err != nil {
				t.Fatalf("write: %v\ninput: %q", err, in)
			}
			if gotOut != c.out {
				t.Fatalf("stream\ninput: %q\ngot:   %q\nwant:  %q", in, gotOut, c.out)
			}
			if strings.Join(gotTitles, "\x00") != strings.Join(c.titles, "\x00") {
				t.Fatalf("titles\ninput: %q\ngot:   %q\nwant:  %q", in, gotTitles, c.titles)
			}
			// The table doubles as the oracle's own check: the
			// reference scan must agree with these hand-written
			// expectations, so the property below is not the
			// implementation compared against itself.
			refOut, refTitles := titleWant(in)
			if refOut != c.out || strings.Join(refTitles, "\x00") != strings.Join(c.titles, "\x00") {
				t.Fatalf("reference model disagrees\ninput: %q\ngot:   %q / %q\nwant:  %q / %q", in, refOut, refTitles, c.out, c.titles)
			}
		})
	}
}

// Every case, cut at every single byte boundary in turn and then one
// byte at a time: a title split across any boundary must still come out
// whole and in order.
func TestTitleFilterSplitAtEveryBoundary(t *testing.T) {
	t.Parallel()
	for _, c := range titleCases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			in := []byte(c.in)
			for i := 1; i < len(in); i++ {
				titleCheck(t, fmt.Sprintf("cut at %d", i), in, titleChunk(in, []int{i}))
			}
			var single [][]byte
			for i := range in {
				single = append(single, in[i:i+1])
			}
			titleCheck(t, "byte at a time", in, single)
		})
	}
}

// Every shape at once, byte at a time, so filter state carries across
// case boundaries the way it does on a live PTY.
func TestTitleFilterMixedStreamByteAtATime(t *testing.T) {
	t.Parallel()
	var b strings.Builder
	for _, c := range titleCases {
		b.WriteString(c.in)
		// Close anything a case deliberately left open so the next
		// case does not start mid-sequence by accident.
		b.WriteString("\x07")
	}
	in := []byte(b.String())
	var single [][]byte
	for i := range in {
		single = append(single, in[i:i+1])
	}
	titleCheck(t, "concatenated cases", in, single)
}

// titleStream draws a stream out of the pieces the filter has to tell
// apart, so payloads really do carry ESC, C1 bytes and semicolons.
func titleStream(rt *rapid.T) []byte {
	payload := rapid.SliceOfN(rapid.SampledFrom([]byte(
		"ab; \x1b\x9c\x9b\x85\x80\xff\r\n[]0289\\")), 0, 12)
	piece := rapid.OneOf(
		rapid.Map(rapid.StringMatching(`[a-zA-Z0-9 \r\n]{0,8}`), func(s string) []byte { return []byte(s) }),
		rapid.Custom(func(rt *rapid.T) []byte {
			cmd := rapid.SampledFrom([]string{"0", "2"}).Draw(rt, "titlecmd")
			p := payload.Draw(rt, "title")
			end := rapid.SampledFrom([][]byte{{0x07}, {0x1b, '\\'}}).Draw(rt, "end")
			return append(append([]byte("\x1b]"+cmd+";"), p...), end...)
		}),
		rapid.Custom(func(rt *rapid.T) []byte {
			cmd := rapid.SampledFrom([]string{"8;;http://x", "11;?", "52;c;AA", "777", "20;y"}).Draw(rt, "osccmd")
			end := rapid.SampledFrom([][]byte{{0x07}, {0x1b, '\\'}}).Draw(rt, "end")
			return append([]byte("\x1b]"+cmd), end...)
		}),
		rapid.SampledFrom([][]byte{
			[]byte("\x1b[1m"), []byte("\x1b[0m"), []byte("\x1b"), []byte("\x1b\x1b"),
			[]byte("\x1b]2;"), []byte("\x07"), []byte("\x1b\\"), []byte("\x9c"),
		}),
	)
	var out []byte
	for _, p := range rapid.SliceOfN(piece, 0, 10).Draw(rt, "pieces") {
		out = append(out, p...)
	}
	return out
}

// For ANY chunking of ANY stream the output equals the stream with
// OSC 0/2 removed, and the titles come back in order.
func TestTitleFilterProperty(t *testing.T) {
	if os.Getenv("BOUGH_FUZZ_TITLE") == "" {
		t.Skip("set BOUGH_FUZZ_TITLE=1")
	}
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		in := titleStream(rt)
		cuts := rapid.SliceOfN(rapid.IntRange(0, len(in)), 0, 8).Draw(rt, "cuts")
		slices.Sort(cuts)
		chunks := titleChunk(in, cuts)

		wantOut, wantTitles := titleWant(in)
		gotOut, gotTitles, err := titleRun(chunks)
		if err != nil {
			rt.Fatalf("write: %v\ninput:  %q\nchunks: %q", err, in, chunks)
		}
		if gotOut != wantOut {
			rt.Fatalf("stream\ninput:  %q\nchunks: %q\ngot:    %q\nwant:   %q", in, chunks, gotOut, wantOut)
		}
		if strings.Join(gotTitles, "\x00") != strings.Join(wantTitles, "\x00") {
			rt.Fatalf("titles\ninput:  %q\nchunks: %q\ngot:    %q\nwant:   %q", in, chunks, gotTitles, wantTitles)
		}
		// Chunking is invisible: one Write gives the same answer.
		oneOut, oneTitles, err := titleRun([][]byte{in})
		if err != nil {
			rt.Fatalf("single write: %v\ninput: %q", err, in)
		}
		if oneOut != gotOut || strings.Join(oneTitles, "\x00") != strings.Join(gotTitles, "\x00") {
			rt.Fatalf("chunking changed the result\ninput:  %q\nchunks: %q\nchunked: %q / %q\nsingle:  %q / %q",
				in, chunks, gotOut, gotTitles, oneOut, oneTitles)
		}
	})
}
