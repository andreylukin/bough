package tools

import (
	"context"
	"fmt"
	"os/exec"
	"regexp"
	"strings"
	"sync"
	"syscall"
	"time"
)

// A background job is a shell command that outlives the code block
// that started it — and the turn. tools.bash(cmd) still runs in the
// foreground and is killed after bashTimeout; tools.bash(cmd, limit)
// detaches the command, returns a handle immediately, and lets it run
// for up to limit. When it exits (or when its output first matches the
// optional `until` pattern) a notice is queued; the loop delivers
// pending notices before its next model step, and wakes an idle agent
// through the Wake channel. That is the "trigger the agent" path: a
// build, a test suite, a server log can bring the agent back on its
// own.

// jobHead/jobTail bound one job's captured output: the head is what
// the command announced, the tail is where it failed. The middle of a
// chatty build is the part nobody reads.
const (
	jobHead = 16 * 1024
	jobTail = 48 * 1024
)

// defaultJobLimit is the kill deadline when tools.bash is given a
// limit that does not parse as a duration.
const defaultJobLimit = 30 * time.Minute

// maxJobLimit caps a job's life so a runaway command cannot outlive
// the session unnoticed.
const maxJobLimit = 6 * time.Hour

type job struct {
	id      int
	cmd     string
	limit   time.Duration
	until   *regexp.Regexp // optional: notify as soon as the output matches
	started time.Time

	mu      sync.Mutex
	head    []byte
	tail    []byte
	cut     int  // bytes dropped between head and tail
	matched bool // the until pattern already fired
	done    bool
	exit    int
	err     string
	ended   time.Time
	cancel  context.CancelFunc
}

// write appends output, keeping the head and a bounded tail.
func (j *job) write(p []byte) {
	if n := jobHead - len(j.head); n > 0 {
		if n > len(p) {
			n = len(p)
		}
		j.head = append(j.head, p[:n]...)
		p = p[n:]
	}
	if len(p) == 0 {
		return
	}
	j.tail = append(j.tail, p...)
	if over := len(j.tail) - jobTail; over > 0 {
		j.tail = j.tail[over:]
		j.cut += over
	}
}

// output is everything kept, with the dropped middle marked.
func (j *job) output() string {
	if j.cut == 0 {
		return string(j.head) + string(j.tail)
	}
	return fmt.Sprintf("%s\n… [%d bytes cut] …\n%s", j.head, j.cut, j.tail)
}

func (j *job) elapsed() time.Duration {
	if j.done {
		return j.ended.Sub(j.started).Round(time.Second)
	}
	return time.Since(j.started).Round(time.Second)
}

func (j *job) status() string {
	switch {
	case !j.done:
		return "running"
	case j.err != "":
		return "failed"
	default:
		return "exited 0"
	}
}

// line is the one-line summary used by tools.jobs and by the notices.
func (j *job) line() string {
	s := fmt.Sprintf("job %d [%s] %s (%s)", j.id, j.status(), firstLine(j.cmd), j.elapsed())
	if j.err != "" {
		s += ": " + j.err
	}
	return s
}

func firstLine(s string) string {
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = strings.TrimSpace(s[:i]) + " …"
	}
	if len(s) > 80 {
		s = s[:80] + "…"
	}
	return s
}

// Jobs is the "job-notices" service: the background jobs of this
// session and the notices they have queued for the agent.
type Jobs struct {
	ctx   context.Context // the plugin's context: a job survives a turn cancel
	pause func() func()   // codemode's Pause seam, for jobWait

	mu      sync.Mutex
	next    int
	list    []*job
	pending []string
	wake    chan struct{} // buffered 1: a signal, not a queue
}

func newJobs(ctx context.Context) *Jobs {
	return &Jobs{ctx: ctx, wake: make(chan struct{}, 1)}
}

// Take drains the queued notices (the loop's Notices seam).
func (j *Jobs) Take() []string {
	j.mu.Lock()
	defer j.mu.Unlock()
	p := j.pending
	j.pending = nil
	return p
}

// Wake fires when a notice has been queued: the loop selects on it so
// a job that finishes while the agent is idle still starts a turn.
func (j *Jobs) Wake() <-chan struct{} { return j.wake }

func (j *Jobs) notify(text string) {
	j.mu.Lock()
	j.pending = append(j.pending, text)
	j.mu.Unlock()
	select {
	case j.wake <- struct{}{}:
	default:
	}
}

func (j *Jobs) find(id int) *job {
	j.mu.Lock()
	defer j.mu.Unlock()
	for _, x := range j.list {
		if x.id == id {
			return x
		}
	}
	return nil
}

// jobLimit reads the limit argument: seconds as a number, or a Go
// duration string ("10m", "2h").
func jobLimit(v any) (time.Duration, error) {
	switch n := v.(type) {
	case int64:
		return time.Duration(n) * time.Second, nil
	case int:
		return time.Duration(n) * time.Second, nil
	case float64:
		return time.Duration(n * float64(time.Second)), nil
	case string:
		if d, err := time.ParseDuration(n); err == nil {
			return d, nil
		}
		return 0, fmt.Errorf("bash: limit %q is not a duration (try 600 or \"10m\")", n)
	}
	return defaultJobLimit, nil
}

// start detaches cmd as a background job.
func (j *Jobs) start(cmd string, limit time.Duration, until string) (*job, error) {
	if limit <= 0 {
		return nil, fmt.Errorf("bash: limit must be positive")
	}
	if limit > maxJobLimit {
		limit = maxJobLimit
	}
	var re *regexp.Regexp
	if until != "" {
		var err error
		if re, err = regexp.Compile(until); err != nil {
			return nil, fmt.Errorf("bash: until %q is not a valid regexp: %v", until, err)
		}
	}
	ctx, cancel := context.WithTimeout(j.ctx, limit)
	c := exec.CommandContext(ctx, "sh", "-s")
	c.Stdin = strings.NewReader(cmd)
	c.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	c.Cancel = func() error { return syscall.Kill(-c.Process.Pid, syscall.SIGKILL) }
	c.WaitDelay = 2 * time.Second

	j.mu.Lock()
	j.next++
	b := &job{id: j.next, cmd: cmd, limit: limit, until: re, started: time.Now(), cancel: cancel}
	j.list = append(j.list, b)
	j.mu.Unlock()

	c.Stdout = &jobWriter{j: j, b: b}
	c.Stderr = c.Stdout
	if err := c.Start(); err != nil {
		cancel()
		b.mu.Lock()
		b.done, b.err, b.ended = true, err.Error(), time.Now()
		b.mu.Unlock()
		return nil, fmt.Errorf("bash: %v", err)
	}
	go func() {
		err := c.Wait()
		cancel()
		b.mu.Lock()
		b.done, b.ended = true, time.Now()
		switch {
		case ctx.Err() == context.DeadlineExceeded:
			b.err, b.exit = "killed after "+limit.String(), -1
		case err != nil:
			b.err, b.exit = err.Error(), -1
			if ee, ok := err.(*exec.ExitError); ok {
				b.exit = ee.ExitCode()
			}
		}
		line, out := b.line(), b.output()
		b.mu.Unlock()
		j.notify(line + "\n" + tailLines(out, 40))
	}()
	return b, nil
}

// jobWriter feeds a job's capture buffer and fires its until pattern.
type jobWriter struct {
	j *Jobs
	b *job
}

func (w *jobWriter) Write(p []byte) (int, error) {
	w.b.mu.Lock()
	w.b.write(p)
	fire := ""
	if w.b.until != nil && !w.b.matched && w.b.until.Match(p) {
		w.b.matched = true
		fire = fmt.Sprintf("job %d matched %q while running: %s\n%s",
			w.b.id, w.b.until.String(), firstLine(w.b.cmd), tailLines(string(p), 20))
	}
	w.b.mu.Unlock()
	if fire != "" {
		w.j.notify(fire)
	}
	return len(p), nil
}

func tailLines(s string, n int) string {
	lines := strings.Split(strings.TrimRight(s, "\n"), "\n")
	if len(lines) <= n {
		return strings.Join(lines, "\n")
	}
	return fmt.Sprintf("… [%d earlier lines] …\n%s", len(lines)-n, strings.Join(lines[len(lines)-n:], "\n"))
}

// jobs lists this session's background jobs.
func (j *Jobs) jobs() (string, error) {
	j.mu.Lock()
	list := append([]*job(nil), j.list...)
	j.mu.Unlock()
	if len(list) == 0 {
		return "no background jobs", nil
	}
	var b strings.Builder
	for _, x := range list {
		x.mu.Lock()
		b.WriteString(x.line() + "\n")
		x.mu.Unlock()
	}
	return strings.TrimRight(b.String(), "\n"), nil
}

// job returns one job's status and everything it has printed.
func (j *Jobs) job(id int) (string, error) {
	b := j.find(id)
	if b == nil {
		return "", fmt.Errorf("no job %d (tools.jobs() lists them)", id)
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.line() + "\n" + b.output(), nil
}

// jobWait blocks until the job exits or wait seconds pass (default:
// the job's own limit). The script timeout is paused meanwhile.
func (j *Jobs) jobWait(id int, secs ...int) (string, error) {
	b := j.find(id)
	if b == nil {
		return "", fmt.Errorf("no job %d (tools.jobs() lists them)", id)
	}
	deadline := time.Now().Add(b.limit)
	if len(secs) > 0 && secs[0] > 0 {
		deadline = time.Now().Add(time.Duration(secs[0]) * time.Second)
	}
	if j.pause != nil {
		defer j.pause()()
	}
	for {
		b.mu.Lock()
		done := b.done
		b.mu.Unlock()
		if done || time.Now().After(deadline) {
			break
		}
		select {
		case <-j.ctx.Done():
			return "", fmt.Errorf("bash: cancelled waiting on job %d", id)
		case <-time.After(200 * time.Millisecond):
		}
	}
	return j.job(id)
}

// jobKill stops a running job.
func (j *Jobs) jobKill(id int) (string, error) {
	b := j.find(id)
	if b == nil {
		return "", fmt.Errorf("no job %d (tools.jobs() lists them)", id)
	}
	b.mu.Lock()
	done, cancel := b.done, b.cancel
	b.mu.Unlock()
	if done {
		return fmt.Sprintf("job %d already finished", id), nil
	}
	cancel()
	return fmt.Sprintf("job %d killed", id), nil
}
