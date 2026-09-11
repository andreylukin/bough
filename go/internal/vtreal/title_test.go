package vtreal

import (
	"bytes"
	"testing"
)

func TestTitleFilterLiftsSplitUTF8Titles(t *testing.T) {
	var out bytes.Buffer
	var titles []string
	f := newTitleFilter(&out, func(s string) { titles = append(titles, s) })
	in := []byte("ab\x1b]2;✓ hikey\x07cd\x1b]0;x\x1b\\e\x1b[1mF\x1b]8;;http://x\x07g")
	// Byte at a time: every sequence crosses a Write boundary.
	for i := range in {
		if _, err := f.Write(in[i : i+1]); err != nil {
			t.Fatal(err)
		}
	}
	if got := out.String(); got != "abcde\x1b[1mF\x1b]8;;http://x\x07g" {
		t.Fatalf("stream: %q", got)
	}
	if len(titles) != 2 || titles[0] != "✓ hikey" || titles[1] != "x" {
		t.Fatalf("titles: %q", titles)
	}
	// A forwarded OSC stays in place: the text before it in the same
	// write must not arrive after it.
	out.Reset()
	f.Write([]byte("A\x1b]11;?\x07B"))
	if got := out.String(); got != "A\x1b]11;?\x07B" {
		t.Fatalf("order: %q", got)
	}
}
