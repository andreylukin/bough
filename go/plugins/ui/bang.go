package ui

// "!" bash mode: a composer line starting with "!" runs the rest
// directly as `sh -c` (60s timeout, cwd = process cwd) — it NEVER
// reaches the LLM. The line echoes like a dispatched command and the
// output lands as a collapsible result-style block labeled "! <cmd>".
// Both halves are recorded to history as "command"/"system" entries
// (model-invisible under DefaultProject, replayable). Headless mode
// prints "[system] <output>" instead (see headless.go).

import (
	"bytes"
	"context"
	"io"
	"os/exec"
	"strings"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"
)

const bangTimeout = 60 * time.Second

// bangCmd strips the "!" prefix off a composer line.
func bangCmd(line string) string {
	return strings.TrimSpace(strings.TrimPrefix(line, "!"))
}

// runBang executes one "!" command. The result is always renderable
// text: combined output, with a loud trailing "! <reason>" line on a
// non-zero exit or timeout, and "(no output)" for silence.
func runBang(cmd string) string { return runBangTo(cmd, nil) }

// runBangTo is runBang that also copies the combined output to w (if
// non-nil) as it arrives, so the TUI can stream a running command.
func runBangTo(cmd string, w io.Writer) string {
	ctx, cancel := context.WithTimeout(context.Background(), bangTimeout)
	defer cancel()
	var out bytes.Buffer
	c := exec.CommandContext(ctx, "sh", "-c", cmd)
	c.Stdout = &out
	if w != nil {
		c.Stdout = io.MultiWriter(&out, w)
	}
	c.Stderr = c.Stdout
	err := c.Run()
	s := strings.TrimRight(out.String(), "\n")
	note := ""
	switch {
	case ctx.Err() == context.DeadlineExceeded:
		note = "! timeout after " + bangTimeout.String()
	case err != nil:
		note = "! " + err.Error()
	}
	switch {
	case note == "" && s == "":
		return "(no output)"
	case note == "":
		return s
	case s == "":
		return note
	}
	return s + "\n" + note
}

// bangRun is one running "!" command: its output so far, a wake
// channel poked on every write, and the final result once it exits
// (exited closes then too, releasing a pending chunk wait).
type bangRun struct {
	line     string
	id       int // the result block streaming this run's output
	mu       sync.Mutex
	buf      []byte
	wake     chan struct{}
	done     chan string
	exited   chan struct{}
	finished bool // set by finishBang (UI goroutine): late chunks are dropped
}

func (r *bangRun) Write(p []byte) (int, error) {
	r.mu.Lock()
	r.buf = append(r.buf, p...)
	r.mu.Unlock()
	select {
	case r.wake <- struct{}{}:
	default:
	}
	return len(p), nil
}

func (r *bangRun) partial() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return strings.TrimRight(string(r.buf), "\n")
}

// wait blocks until the run exits.
func (r *bangRun) wait() tea.Msg {
	return bangDoneMsg{line: r.line, id: r.id, out: <-r.done, run: r}
}

// next waits for the run's next output; nil once it has exited.
func (r *bangRun) next() tea.Msg {
	select {
	case <-r.wake:
		return bangChunkMsg{run: r}
	case <-r.exited:
		return nil
	}
}

// bangChunkMsg says a running "!" command wrote more output.
type bangChunkMsg struct{ run *bangRun }

// bangDoneMsg delivers a finished "!" command's output to Update.
type bangDoneMsg struct {
	line string // the full "!" composer line
	id   int    // the result block to settle
	out  string
	run  *bangRun
}

// dispatchBang handles a submitted "!" line: echo + history "command"
// entry and a labeled result block now, then the shell run in the
// background, its output streamed into that block as it arrives so a
// slow command never freezes the UI or hides its progress.
func (m *model) dispatchBang(line string) tea.Cmd {
	cfg := m.cfg.Load()
	m.input.Reset()
	m.syncPalette()
	m.log(cfg, "command", line)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "command", text: line})
	m.nextID++
	r := &bangRun{line: line, id: m.nextID, wake: make(chan struct{}, 1), done: make(chan string, 1), exited: make(chan struct{})}
	m.blocks = append(m.blocks, block{id: r.id, kind: "result", label: "! " + bangCmd(line), text: "running…"})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	cmd := bangCmd(line)
	go func() {
		r.done <- runBangTo(cmd, r)
		close(r.exited)
	}()
	return tea.Batch(r.wait, r.next)
}

// streamBang shows a running command's output so far and waits for more.
func (m *model) streamBang(msg bangChunkMsg) tea.Cmd {
	if msg.run.finished {
		return nil
	}
	if s := msg.run.partial(); s != "" {
		m.setBangText(msg.run.id, s)
	}
	return msg.run.next
}

func (m *model) setBangText(id int, text string) {
	for i := range m.blocks {
		if m.blocks[i].id == id {
			m.blocks[i].text = text
			m.refresh()
			m.vp.GotoBottom()
			return
		}
	}
}

// finishBang records the "system" history entry and settles the
// labeled result block on the final output. The user asked for this
// output, so it starts expanded (still collapsible like any result block).
func (m *model) finishBang(msg bangDoneMsg) {
	cfg := m.cfg.Load()
	m.log(cfg, "system", msg.out)
	msg.run.finished = true
	m.setBangText(msg.id, msg.out)
}
