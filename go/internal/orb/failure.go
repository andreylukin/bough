package orb

import (
	"regexp"
	"strings"
)

// maxErrorLines is how many log lines a failure message carries.
const maxErrorLines = 6

// tailBuffer keeps the last 64 KiB written to it: enough for a failure's
// last lines without holding a whole build log.
type tailBuffer struct{ b []byte }

func (t *tailBuffer) Write(p []byte) (int, error) {
	const max = 64 << 10
	t.b = append(t.b, p...)
	if len(t.b) > max {
		t.b = append([]byte(nil), t.b[len(t.b)-max:]...)
	}
	return len(p), nil
}

func (t *tailBuffer) String() string { return string(t.b) }

var (
	ansiRE  = regexp.MustCompile(`\x1b\[[0-9;?]*[A-Za-z]`)
	errorRE = regexp.MustCompile(`(?i)\berr(or)?\b|^\S*\s*(\d+(\.\d+)?\s+)?E: |fail|not found|no such|denied|cannot|can't|unable|fatal|panic|exit code`)
)

// ErrorLines picks the log lines that say why a build or resume.sh failed:
// the last n lines that look like errors, or the last n lines when none
// does. bough's own markers and blank or progress-only lines are skipped.
func ErrorLines(log string, n int) []string {
	var lines []string
	for _, l := range strings.Split(ansiRE.ReplaceAllString(log, ""), "\n") {
		if i := strings.LastIndex(l, "\r"); i >= 0 && strings.TrimSpace(l[i+1:]) != "" {
			l = l[i+1:] // a progress line redrawn in place: its last state
		}
		l = strings.TrimSpace(l)
		if l == "" || strings.HasPrefix(l, "== resume.sh ") || strings.HasPrefix(l, "build failed: ") {
			continue
		}
		lines = append(lines, l)
	}
	var hits []string
	for _, l := range lines {
		if errorRE.MatchString(l) {
			hits = append(hits, l)
		}
	}
	if len(hits) == 0 {
		hits = lines
	}
	if len(hits) > n {
		hits = hits[len(hits)-n:]
	}
	return hits
}

// logError is a failure with the log lines that explain it.
type logError struct {
	err   error
	lines []string
	path  string
}

func (e *logError) Error() string {
	var b strings.Builder
	b.WriteString(e.err.Error())
	for _, l := range e.lines {
		b.WriteString("\n  ")
		b.WriteString(l)
	}
	b.WriteString("\nfull log: ")
	b.WriteString(e.path)
	return b.String()
}

func (e *logError) Unwrap() error { return e.err }

func withLogLines(err error, log, path string) error {
	return &logError{err: err, lines: ErrorLines(log, maxErrorLines), path: path}
}
