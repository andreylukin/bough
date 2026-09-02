package ui

// "!" bash mode: a composer line starting with "!" runs the rest
// directly as `sh -c` (60s timeout, cwd = process cwd) — it NEVER
// reaches the LLM. The line echoes like a dispatched command and the
// output lands as a collapsible result-style block labeled "! <cmd>".
// Both halves are recorded to history as "command"/"system" entries
// (model-invisible under DefaultProject, replayable). Headless mode
// prints "[system] <output>" instead (see headless.go).

import (
	"context"
	"os/exec"
	"strings"
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
	ctx, cancel := context.WithTimeout(context.Background(), bangTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "sh", "-c", cmd).CombinedOutput()
	s := strings.TrimRight(string(out), "\n")
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

// bangDoneMsg delivers a finished "!" command's output to Update.
type bangDoneMsg struct {
	line string // the full "!" composer line
	out  string
}

// dispatchBang handles a submitted "!" line: echo + history "command"
// entry now, then the shell run as a tea.Cmd so a slow command never
// freezes the UI.
func (m *model) dispatchBang(line string) tea.Cmd {
	cfg := m.cfg.Load()
	m.input.Reset()
	m.syncPalette()
	m.log(cfg, "command", line)
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "command", text: line})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	cmd := bangCmd(line)
	return func() tea.Msg {
		return bangDoneMsg{line: line, out: runBang(cmd)}
	}
}

// finishBang records the "system" history entry and appends the
// labeled result block. The user asked for this output, so it starts
// expanded (still collapsible like any result block).
func (m *model) finishBang(msg bangDoneMsg) {
	cfg := m.cfg.Load()
	m.log(cfg, "system", msg.out)
	m.blocks = append(m.blocks, block{
		id: m.nextID, kind: "result", label: "! " + bangCmd(msg.line), text: msg.out,
	})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
}
