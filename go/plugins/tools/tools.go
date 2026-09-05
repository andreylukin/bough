// Package tools is the "tools-basic" plugin: bash, view, patch
// registered into the codemode service. It also provides "turn-stats":
// the files written and the last bash exit code since the last Take,
// which the loop stamps onto its end-of-turn "done" entry.
package tools

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// bashTimeout is the tools.bash kill deadline (documented in the loop's
// system prompt; a var so tests can shorten it).
var bashTimeout = 60 * time.Second

// registry is the slice of the codemode service we need.
type registry interface {
	RegisterTool(name string, fn any)
}

// describer is codemode's optional prompt-catalogue seam: a tool
// documents itself where it is registered, so the model is never told
// about a tool that is not mounted (or left ignorant of one that is).
type describer interface {
	Describe(name, line string)
}

// runContexter is the optional slice of codemode that exposes the
// running script's context: the turn's cancel reaches tools.bash
// through it.
type runContexter interface {
	RunContext() context.Context
}

// pauser is codemode's seam for a tool that blocks longer than the
// script timeout (tools.ask uses it too); tools.jobWait needs it.
type pauser interface{ Pause() func() }

// Stats is the "turn-stats" service: side-effect tallies of the basic
// tools, reset by Take.
type Stats struct {
	runCtx func() context.Context // the running script's context; nil = none
	jobs   *Jobs                  // background jobs (never nil after Apply)

	mu    sync.Mutex
	files []string
	exit  int
	ran   bool // a bash call happened since the last Take
	// read remembers what each path looked like when it was last
	// viewed THIS turn, so a re-read that found nothing new can say so
	// (see view). Cleared by Take, like the rest of the turn's tally.
	read map[string]string
}

// Take returns the files written and the last bash exit code (ran is
// false when no bash call happened) since the previous Take, and resets.
func (s *Stats) Take() (files []string, exit int, ran bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	files, exit, ran = s.files, s.exit, s.ran
	s.files, s.exit, s.ran, s.read = nil, 0, false, nil
	return files, exit, ran
}

// wrote records a file this turn touched, once: a turn that edits the
// same file three times ended with "✔ wrote llm.go, llm.go, llm.go".
func (s *Stats) wrote(path string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if slices.Contains(s.files, path) {
		return
	}
	s.files = append(s.files, path)
}

func (s *Stats) exited(code int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.exit, s.ran = code, true
}

type plugin struct{}

func init() {
	kernel.Register("tools-basic", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "tools-basic" }
func (plugin) Inject() []string { return []string{"codemode"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	reg, err := kernel.Get[registry](ctx, "codemode")
	if err != nil {
		return err
	}
	st := &Stats{}
	if rc, ok := reg.(runContexter); ok {
		st.runCtx = rc.RunContext
	}
	// A background job outlives the turn that started it, so it hangs
	// off the plugin's context, not the script's.
	jctx, cancelJobs := context.WithCancel(context.Background())
	ctx.Effect(cancelJobs)
	st.jobs = newJobs(jctx)
	if p, ok := reg.(pauser); ok {
		st.jobs.pause = p.Pause
	}
	ctx.Provide("job-notices", st.jobs)
	if d, ok := reg.(describer); ok {
		for _, doc := range [][2]string{
			{"bash", `tools.bash(cmd) -> string: run a shell command, returns its output. Killed after 60 s (the error says so).`},
			{"bash-bg", `tools.bash(cmd, limit[, until]) -> string: the same command in the BACKGROUND — limit is seconds (or "10m"), the call returns a job id at once, and you are told when it exits or when its output matches the regexp until. Use it for anything longer than the 60 s foreground kill: a test suite, a build, a server you need up while you work.`},
			{"jobs", `tools.jobs() -> string: the background jobs and their state.`},
			{"job", `tools.job(id) -> string: one job's status and output so far.`},
			{"jobWait", `tools.jobWait(id, [seconds]) -> string: block until a job exits.`},
			{"jobKill", `tools.jobKill(id) -> string: stop a job.`},
			{"view", `tools.view(path, [start, end]) -> string: a file's lines, numbered ("12│text"); optional 1-based inclusive range.`},
			{"write", `tools.write(path, content) -> string: create or overwrite a whole file (use this for new files and rewrites, never a shell heredoc).`},
			{"patch", `tools.patch(path, old, new) -> string: replace ONE exact occurrence of old with new (copy old verbatim from view, enough lines to be unique).`},
		} {
			d.Describe(doc[0], doc[1])
		}
	}
	reg.RegisterTool("bash", st.bash)
	reg.RegisterTool("jobs", st.jobs.jobs)
	reg.RegisterTool("job", st.jobs.job)
	reg.RegisterTool("jobWait", st.jobs.jobWait)
	reg.RegisterTool("jobKill", st.jobs.jobKill)
	reg.RegisterTool("view", st.view)
	reg.RegisterTool("patch", st.patch)
	reg.RegisterTool("write", st.write)
	ctx.Provide("turn-stats", st)
	return nil
}

// bash runs cmd. With no limit it runs in the foreground and is killed
// after bashTimeout; given a limit (seconds, or a duration string) it
// becomes a background job that outlives the turn, and an optional
// third argument is a regexp to watch its output for.
func (s *Stats) bash(cmd string, opts ...any) (string, error) {
	if len(opts) > 0 && opts[0] != nil {
		limit, err := jobLimit(opts[0])
		if err != nil {
			return "", err
		}
		until := ""
		if len(opts) > 1 {
			if u, ok := opts[1].(string); ok {
				until = u
			}
		}
		b, err := s.jobs.start(cmd, limit, until)
		if err != nil {
			return "", err
		}
		msg := fmt.Sprintf("job %d started in the background (limit %s): %s", b.id, limit, firstLine(cmd))
		if until != "" {
			msg += fmt.Sprintf("\nwatching its output for %q", until)
		}
		return msg + "\nYou will be told when it finishes; tools.job(" + strconv.Itoa(b.id) + ") reads it meanwhile.", nil
	}
	parent := context.Background()
	if s.runCtx != nil {
		parent = s.runCtx()
	}
	ctx, cancel := context.WithTimeout(parent, bashTimeout)
	defer cancel()
	// The script goes in on stdin, not as an argument: a heredoc'd file
	// or a long one-liner is not bounded by ARG_MAX, and a stray NUL
	// byte no longer makes exec fail with "invalid argument".
	c := exec.CommandContext(ctx, "sh", "-s")
	c.Stdin = strings.NewReader(cmd)
	// Its own process group, killed as a group: `sh -c` execs or forks
	// the command, and killing sh alone leaves a sleep, a server, a
	// build running after the turn was cancelled.
	ownProcessGroup(c)
	c.Cancel = func() error { return killProcessGroup(c) }
	c.WaitDelay = 2 * time.Second
	out, err := c.CombinedOutput()
	if ctx.Err() == context.DeadlineExceeded {
		s.exited(-1)
		return "", fmt.Errorf("bash: killed after %s: %s\n%s", bashTimeout, cmd, out)
	}
	if ctx.Err() == context.Canceled {
		s.exited(-1)
		return "", fmt.Errorf("bash: cancelled: %s\n%s", cmd, out)
	}
	if err != nil {
		code := -1
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			code = ee.ExitCode()
		}
		s.exited(code)
		// "bash: exit status 1" alone is the header a collapsed error
		// row shows, and it says nothing. Lead with what failed and
		// what it said.
		return "", fmt.Errorf("bash: %s: %v%s", firstLine(cmd), err, tail(string(out)))
	}
	s.exited(0)
	return string(out), nil
}

// tail is a command's output for an error message: its first line on
// the header line (where a collapsed row can show it), then the rest.
func tail(out string) string {
	out = strings.TrimRight(out, "\n")
	if out == "" {
		return ""
	}
	head, rest, _ := strings.Cut(out, "\n")
	if rest == "" {
		return " — " + head
	}
	return " — " + head + "\n" + rest
}

// write creates or overwrites path with content, making parent
// directories. The plain way to put a whole file down: no heredoc
// quoting, no shell at all.
func (s *Stats) write(path, content string) (string, error) {
	before, hadFile := os.ReadFile(path)
	if dir := filepath.Dir(path); dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return "", err
		}
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	out := fmt.Sprintf("wrote %s (%d bytes, %d lines)", path, len(content), strings.Count(content, "\n")+1)
	if hadFile == nil {
		out += lineDiff(string(before), content)
	}
	return out, nil
}

// diffLimit caps the lines a diff considers; past it the change is
// reported without one (a rewrite of a big file is the code block).
const diffLimit = 400

// lineDiff renders old → new as "\n-old\n+new" lines, a blank line
// after the summary: an LCS over lines, unchanged lines omitted
// except one line of context on each side of a change, "…" for a
// gap between shown lines. "" when the
// texts are equal or either exceeds diffLimit lines. Pure.
func lineDiff(old, new string) string {
	if old == new {
		return ""
	}
	a, b := splitLines(old), splitLines(new)
	if len(a) > diffLimit || len(b) > diffLimit {
		return ""
	}
	// lcs[i][j] = LCS length of a[i:], b[j:].
	lcs := make([][]int, len(a)+1)
	for i := range lcs {
		lcs[i] = make([]int, len(b)+1)
	}
	for i, v := range slices.Backward(a) {
		for j := len(b) - 1; j >= 0; j-- {
			if v == b[j] {
				lcs[i][j] = lcs[i+1][j+1] + 1
			} else {
				lcs[i][j] = max(lcs[i+1][j], lcs[i][j+1])
			}
		}
	}
	type op struct {
		tag  byte
		text string
	}
	var ops []op
	for i, j := 0, 0; i < len(a) || j < len(b); {
		switch {
		case i < len(a) && j < len(b) && a[i] == b[j]:
			ops = append(ops, op{' ', a[i]})
			i++
			j++
		case i < len(a) && (j == len(b) || lcs[i+1][j] >= lcs[i][j+1]):
			ops = append(ops, op{'-', a[i]})
			i++
		default:
			ops = append(ops, op{'+', b[j]})
			j++
		}
	}
	var sb strings.Builder
	sb.WriteString("\n")
	skipped, shown := false, false
	for k, o := range ops {
		if o.tag == ' ' {
			near := (k > 0 && ops[k-1].tag != ' ') || (k+1 < len(ops) && ops[k+1].tag != ' ')
			if !near {
				skipped = true
				continue
			}
		}
		if skipped && shown {
			sb.WriteString("\n…")
		}
		skipped, shown = false, true
		sb.WriteString("\n" + string(o.tag) + o.text)
	}
	return sb.String()
}

// splitLines splits on newlines without a trailing empty element.
func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	return strings.Split(strings.TrimSuffix(s, "\n"), "\n")
}

// unchangedNote is appended when a view returns exactly what the same
// view returned earlier in the same turn.
//
// A real run read one file nineteen times in thirty-eight steps and
// nothing told it so. Re-reading after an edit is normal and this stays
// quiet for it — the note appears only when the bytes are identical,
// which means the step bought nothing.
const unchangedNote = "\n[you already read this in this turn and it has not changed since — it will not change unless something writes to it]"

// view is Stats.view: the read, plus the note when it repeats.
func (s *Stats) view(path string, rng ...int) (string, error) {
	out, err := readView(path, rng...)
	if err != nil {
		return out, err
	}
	key := path
	for _, n := range rng {
		key += ":" + strconv.Itoa(n)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.read == nil {
		s.read = map[string]string{}
	}
	if prev, seen := s.read[key]; seen && prev == out {
		return out + unchangedNote, nil
	}
	s.read[key] = out
	return out, nil
}

// readView returns a file's lines numbered "N│text", optionally only
// lines start..end (1-based, inclusive; end 0 = to the end). Numbers
// make patch targets and error lines easy to refer to.
func readView(path string, rng ...int) (string, error) {
	if st, serr := os.Stat(path); serr == nil && st.IsDir() {
		ents, rerr := os.ReadDir(path)
		if rerr != nil {
			return "", rerr
		}
		names := make([]string, 0, len(ents))
		for _, e := range ents {
			n := e.Name()
			if e.IsDir() {
				n += "/"
			}
			names = append(names, n)
		}
		return fmt.Sprintf("%s is a directory: %s", path, strings.Join(names, " ")), nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", withNeighbours(path, err)
	}
	lines := strings.Split(strings.TrimSuffix(string(data), "\n"), "\n")
	start, end := 1, len(lines)
	if len(rng) > 0 && rng[0] > 0 {
		start = rng[0]
	}
	if len(rng) > 1 && rng[1] > 0 && rng[1] < end {
		end = rng[1]
	}
	if start > len(lines) {
		return "", fmt.Errorf("view: %s has %d lines, start %d is past the end", path, len(lines), start)
	}
	width := len(strconv.Itoa(end))
	var b strings.Builder
	for n := start; n <= end; n++ {
		fmt.Fprintf(&b, "%*d│%s\n", width, n, lines[n-1])
	}
	return b.String(), nil
}

// withNeighbours turns a bare "no such file" into one that names the
// files actually next to the guessed path: a model that invents
// go/plugins/loop/turn.go should be told loop.go and cancel.go exist,
// not left to guess again.
func withNeighbours(path string, err error) error {
	if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	dir := filepath.Dir(path)
	ents, rerr := os.ReadDir(dir)
	if rerr != nil {
		return fmt.Errorf("%w (no directory %s either)", err, dir)
	}
	base := strings.ToLower(strings.TrimSuffix(filepath.Base(path), filepath.Ext(path)))
	var near, all []string
	for _, e := range ents {
		n := e.Name()
		if e.IsDir() {
			n += "/"
		}
		all = append(all, n)
		if l := strings.ToLower(n); strings.Contains(l, base) || strings.Contains(base, strings.TrimSuffix(l, filepath.Ext(l))) {
			near = append(near, n)
		}
	}
	list := near
	if len(list) == 0 {
		list = all
	}
	if len(list) > 12 {
		list = append(list[:12:12], "…")
	}
	if len(list) == 0 {
		return fmt.Errorf("%w (%s is empty)", err, dir)
	}
	return fmt.Errorf("%w — %s holds: %s", err, dir, strings.Join(list, " "))
}

// closestMatch returns the 1-based line of the run of lines (as many
// as old spans) in data most similar to old, and whether the file has
// one: a model that mistyped its old text or copied it from an older
// version of the file should be shown what is nearly there, not left
// to view the whole file and guess again. Ties go to the earliest
// window. Pure.
func closestMatch(data, old string) (int, bool) {
	if old == "" {
		return 0, false
	}
	ls := splitLines(data)
	n := strings.Count(old, "\n") + 1
	if len(ls) < n {
		return 0, false
	}
	best, bestDist := 0, -1
	for i := 0; i+n <= len(ls); i++ {
		w := strings.Join(ls[i:i+n], "\n")
		// |len(a)-len(b)| is a lower bound on the distance: skip
		// windows that cannot beat the best so far, so a long file
		// with a good early match stays cheap.
		gap := len(w) - len(old)
		if gap < 0 {
			gap = -gap
		}
		if bestDist >= 0 && gap >= bestDist {
			continue
		}
		if d := editDistance(old, w); bestDist < 0 || d < bestDist {
			best, bestDist = i+n/2+1, d
		}
	}
	return best, true
}

// editDistance is the Levenshtein distance between a and b: how many
// single-character insertions, deletions or replacements turn a into
// b. Pure.
func editDistance(a, b string) int {
	ar, br := []rune(a), []rune(b)
	prev := make([]int, len(br)+1)
	cur := make([]int, len(br)+1)
	for j := range prev {
		prev[j] = j
	}
	for i := 1; i <= len(ar); i++ {
		cur[0] = i
		for j := 1; j <= len(br); j++ {
			cost := 1
			if ar[i-1] == br[j-1] {
				cost = 0
			}
			cur[j] = min(min(cur[j-1]+1, prev[j]+1), prev[j-1]+cost)
		}
		prev, cur = cur, prev
	}
	return prev[len(br)]
}

// nearestLines renders the lines of data around line at (1-based),
// lines on each side, numbered "N│text" like view: the neighbourhood
// the model should have copied old from. "" when at is outside the
// file.
func nearestLines(data string, at, lines int) string {
	ls := splitLines(data)
	if at < 1 || at > len(ls) {
		return ""
	}
	lo, hi := max(at-lines, 1), min(at+lines, len(ls))
	width := len(strconv.Itoa(hi))
	var b strings.Builder
	for n := lo; n <= hi; n++ {
		fmt.Fprintf(&b, "%*d│%s\n", width, n, ls[n-1])
	}
	return b.String()
}

// patch replaces one exact occurrence of old with new in path. old
// must match exactly once (include more context when it repeats). An
// empty old creates the file with new when it does not exist yet.
func (s *Stats) patch(path, old, new string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil && old != "" {
		err = withNeighbours(path, err)
	}
	if old == "" {
		if err == nil {
			return "", fmt.Errorf("patch: %s exists; give the text to replace (old) or create a new path", path)
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
		if dir := filepath.Dir(path); dir != "." {
			if err := os.MkdirAll(dir, 0o755); err != nil {
				return "", err
			}
		}
		if err := os.WriteFile(path, []byte(new), 0o644); err != nil {
			return "", err
		}
		s.wrote(path)
		return fmt.Sprintf("created %s (%d bytes)", path, len(new)), nil
	}
	if err != nil {
		return "", err
	}
	switch n := strings.Count(string(data), old); n {
	case 0:
		if line, ok := closestMatch(string(data), old); ok {
			if near := nearestLines(string(data), line, 2); near != "" {
				return "", fmt.Errorf("patch: old text not found in %s (view it and copy the exact lines) — closest match near line %d:\n%s", path, line, near)
			}
		}
		return "", fmt.Errorf("patch: old text not found in %s (view it and copy the exact lines)", path)
	case 1:
	default:
		return "", fmt.Errorf("patch: old text occurs %d times in %s; include more surrounding lines", n, path)
	}
	out := strings.Replace(string(data), old, new, 1)
	if err := os.WriteFile(path, []byte(out), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	return fmt.Sprintf("patched %s (%+d lines)", path,
		strings.Count(new, "\n")-strings.Count(old, "\n")) + lineDiff(old, new), nil
}
