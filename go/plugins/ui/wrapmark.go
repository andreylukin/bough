package ui

import (
	"strings"

	xansi "github.com/charmbracelet/x/ansi"
)

// Soft-wrap marks: a zero-width OSC appended to a rendered row that is
// a wrap of the same source line, not a real newline. They ride from
// markdown() through fit() to refresh, which strips them and keeps the
// record so a selection can rejoin wrapped rows. wrapSpace means the
// wrap ate a space; wrapJoin means a long word was cut mid-run.
const (
	wrapSpace = "\x1b]8989;s\x07"
	wrapJoin  = "\x1b]8989;j\x07"
)

// unwrapMarks strips the marks from lines, returning the clean lines
// and, per line, the joiner to its successor ("" for a real newline).
func unwrapMarks(lines []string) ([]string, []string) {
	soft := make([]string, len(lines))
	for i, l := range lines {
		switch {
		case strings.Contains(l, wrapSpace):
			soft[i] = " "
		case strings.Contains(l, wrapJoin):
			soft[i] = "\x00" // joined with nothing
		default:
			continue
		}
		lines[i] = strings.ReplaceAll(strings.ReplaceAll(l, wrapSpace, ""), wrapJoin, "")
	}
	return lines, soft
}

// stripMarks removes the marks from rendered text headed for a surface
// that does not select.
func stripMarks(s string) string {
	return strings.ReplaceAll(strings.ReplaceAll(s, wrapSpace, ""), wrapJoin, "")
}

// markWraps marks the rows of disp (rendered at the pane width) that
// continue the same line of wide (the same text rendered unwrapped).
// When the two do not line up it marks nothing.
func markWraps(disp, wide string) string {
	d := strings.Split(disp, "\n")
	l := strings.Split(wide, "\n")
	marks := make([]string, len(d))
	j, pos := 0, 0
	for i, row := range d {
		for j < len(l) && pos == 0 && strings.TrimSpace(xansi.Strip(l[j])) == "" && strings.TrimSpace(xansi.Strip(row)) != "" {
			j++ // a blank logical line the wrapped render dropped
		}
		if j >= len(l) {
			return disp
		}
		line := xansi.Strip(l[j])
		p := strings.TrimSpace(xansi.Strip(row))
		idx := strings.Index(line[pos:], p)
		if idx < 0 {
			return disp
		}
		pos += idx + len(p)
		if strings.TrimSpace(line[pos:]) == "" || i == len(d)-1 {
			j, pos = j+1, 0
			continue
		}
		if line[pos] == ' ' {
			marks[i] = wrapSpace
		} else {
			marks[i] = wrapJoin
		}
	}
	for i := range d {
		d[i] += marks[i]
	}
	return strings.Join(d, "\n")
}
