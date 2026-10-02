package vtreal

import (
	"strings"
	"testing"
)

func streamPtyTestFrame(cols, rows int) string {
	ls := make([]string, rows)
	for i := range rows - 2 {
		ls[i] = "reply"
	}
	ls[rows-2] = " bough" + strings.Repeat(" ", cols-1-len(" bough? keys")) + "? keys"
	ls[rows-1] = "> say something"
	return strings.Join(ls, "\n")
}

// tmux can synchronously clip or restore cells before the app handles
// SIGWINCH. Neither a composer on the bottom row alone nor the old
// width with the new height acknowledges a completed resize render.
func TestStreamPtyFrameAtSizeRejectsOldCells(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		frame string
		cols  int
		rows  int
		want  bool
	}{
		{"original", streamPtyTestFrame(100, 30), 100, 30, true},
		{"smaller", streamPtyTestFrame(99, 29), 99, 29, true},
		{"clipped old width", streamPtyTestFrame(100, 29), 99, 29, false},
		{"late short frame", streamPtyTestFrame(100, 29) + "\n", 100, 30, false},
		{"restored old width", streamPtyTestFrame(99, 30), 100, 30, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := streamPtyFrameAtSize(tc.frame, tc.cols, tc.rows); got != tc.want {
				t.Fatalf("frame at %dx%d = %v, want %v:\n%s", tc.cols, tc.rows, got, tc.want, tc.frame)
			}
		})
	}
}

type streamPtyOrderedFrames struct {
	t      *testing.T
	frames []string
	settle []string
}

func (s *streamPtyOrderedFrames) waitUntil(pred func(string) bool, what string) {
	s.t.Helper()
	for len(s.frames) > 0 {
		frame := s.frames[0]
		s.frames = s.frames[1:]
		if pred(frame) {
			return
		}
	}
	s.t.Fatalf("never observed %s", what)
}

func (s *streamPtyOrderedFrames) settled() string {
	s.t.Helper()
	if len(s.settle) == 0 {
		s.t.Fatal("settled was called on an unacknowledged resize frame")
	}
	frame := s.settle[0]
	s.settle = s.settle[1:]
	return frame
}

// The old retry exited on a temporarily restored original screen,
// then returned a late smaller render from settled. Even when the
// first size observation matches, settling must still confirm it.
func TestStreamPtySizedFrameWaitsPastLateOldRender(t *testing.T) {
	t.Parallel()
	want := streamPtyTestFrame(100, 30)
	old := streamPtyTestFrame(99, 29) + "\n"
	s := &streamPtyOrderedFrames{t: t, frames: []string{old, want, old, want}, settle: []string{old, want}}
	if got := streamPtySizedFrame(s, 100, 30); got != want {
		t.Fatalf("returned the late smaller render:\n%s", got)
	}
	if len(s.frames) != 0 || len(s.settle) != 0 {
		t.Fatalf("returned before the final acknowledged frame: %d observations and %d settles remain", len(s.frames), len(s.settle))
	}
}
