package ui

// "!" bash mode: a composer line starting with "!" runs the rest
// directly as `sh -c` (60s timeout or esc, cwd = process cwd) — it NEVER
// reaches the LLM. The line echoes like a dispatched command and the
// output lands as a collapsible result-style block labeled "! <cmd>".
// Both halves are recorded to history as "command"/"system" entries
// (model-invisible under DefaultProject, replayable). Headless mode
// prints "[system] <output>" instead (see headless.go).

import (
	"context"
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
func runBang(cmd string) string {
	return runBangCtx(context.Background(), cmd, &bangBuf{})
}

// runBangCtx runs cmd in its own process group, streaming combined
// output into buf; cancelling ctx kills the whole group (sh -c's
// children included) and ends the text with "! cancelled".
func runBangCtx(ctx context.Context, cmd string, buf *bangBuf) string {
	ctx, cancel := context.WithTimeout(ctx, bangTimeout)
	defer cancel()
	c := exec.CommandContext(ctx, "sh", "-c", cmd)
	c.Stdout, c.Stderr = buf, buf
	ownProcessGroup(c)
	c.Cancel = func() error { return killProcessGroup(c) }
	c.WaitDelay = time.Second
	err := c.Run()
	s := strings.TrimRight(buf.String(), "\n")
	note := ""
	switch {
	case ctx.Err() == context.DeadlineExceeded:
		note = "! timeout after " + bangTimeout.String()
	case ctx.Err() != nil:
		note = "! cancelled"
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

// bangBuf is the concurrency-safe output sink a running "!" command
// writes and the UI's tick reads.
type bangBuf struct {
	mu sync.Mutex
	b  []byte
}

func (b *bangBuf) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.b = append(b.b, p...)
	return len(p), nil
}

func (b *bangBuf) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return string(b.b)
}

// bangRun is the in-flight "!" command: esc cancels it, ticks copy
// its partial output into its result block.
type bangRun struct {
	id     int // the result block
	buf    *bangBuf
	cancel context.CancelFunc
	exited chan struct{} // closed once the shell and its group are gone
}

// stopBang kills a still-running "!" command's process group and waits
// (bounded) for it: quitting must not leave its tree running.
func (m model) stopBang() {
	if m.bang == nil {
		return
	}
	m.bang.cancel()
	select {
	case <-m.bang.exited:
	case <-time.After(3 * time.Second):
	}
}

// bangDoneMsg delivers a finished "!" command's output to Update.
type bangDoneMsg struct {
	line string // the full "!" composer line
	out  string
	run  *bangRun
}

// bangTickMsg refreshes a running "!" block's partial output.
type bangTickMsg struct{ run *bangRun }

const bangTick = 100 * time.Millisecond

// dispatchBang handles a submitted "!" line: echo + history "command"
// entry now, then the shell run as a tea.Cmd so a slow command never
// freezes the UI. The result block exists from the start and streams.
func (m *model) dispatchBang(line string) tea.Cmd {
	cfg := m.cfg.Load()
	m.input.Reset()
	m.syncPalette()
	m.log(cfg, "command", line)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "command", text: line})
	m.nextID++
	cmd := bangCmd(line)
	ctx, cancel := context.WithCancel(context.Background())
	run := &bangRun{id: m.nextID, buf: &bangBuf{}, cancel: cancel, exited: make(chan struct{})}
	m.bang = run
	m.blocks = append(m.blocks, block{id: run.id, kind: "result", label: "! " + cmd, text: "…"})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	return tea.Batch(func() tea.Msg {
		defer cancel()
		defer close(run.exited)
		return bangDoneMsg{line: line, out: runBangCtx(ctx, cmd, run.buf), run: run}
	}, bangTickCmd(run))
}

func bangTickCmd(run *bangRun) tea.Cmd {
	return tea.Tick(bangTick, func(time.Time) tea.Msg { return bangTickMsg{run: run} })
}

// setBangText replaces the run's result block text.
func (m *model) setBangText(run *bangRun, text string) {
	for i := range m.blocks {
		if m.blocks[i].id == run.id {
			m.blocks[i].text = text
		}
	}
	m.refresh()
	m.vp.GotoBottom()
}

// tickBang copies partial output in while the run is still current.
func (m *model) tickBang(msg bangTickMsg) tea.Cmd {
	if m.bang != msg.run {
		return nil
	}
	if s := strings.TrimRight(msg.run.buf.String(), "\n"); s != "" {
		m.setBangText(msg.run, s)
	}
	return bangTickCmd(msg.run)
}

// finishBang records the "system" history entry and fills the
// labeled result block. The user asked for this output, so it starts
// expanded (still collapsible like any result block).
func (m *model) finishBang(msg bangDoneMsg) {
	cfg := m.cfg.Load()
	m.log(cfg, "system", msg.out)
	if m.bang == msg.run {
		m.bang = nil
	}
	m.setBangText(msg.run, msg.out)
}
