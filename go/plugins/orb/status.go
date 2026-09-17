package orb

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/tools"
)

// progress rewrites one line on w with the start's current phase, read
// from state.json every tick, until the returned stop clears it. It is
// what a TUI session shows while the orb builds, before the TUI is up.
func progress(w io.Writer, home, session string, tick time.Duration) (stop func()) {
	done := make(chan struct{})
	finished := make(chan struct{})
	go func() {
		defer close(finished)
		t := time.NewTicker(tick)
		defer t.Stop()
		for {
			if st, err := iorb.ReadState(home, session); err == nil && st.Session != "" {
				fmt.Fprint(w, "\r\x1b[K"+iorb.PhaseLine(st, time.Now()))
			}
			select {
			case <-done:
				fmt.Fprint(w, "\r\x1b[K")
				return
			case <-t.C:
			}
		}
	}()
	return func() { close(done); <-finished }
}

const logTail = 40

// orbCommand is /orb in a project session: status, logs and stop, while
// any other argument still runs the /orb setup skill.
func orbCommand(o *iorb.Orb, home string, jobs func() []tools.Running) func(string) (string, error) {
	return func(args string) (string, error) {
		verb, rest, _ := strings.Cut(strings.TrimSpace(args), " ")
		switch verb {
		case "status":
			return orbStatus(o.State(), time.Now()), nil
		case "logs":
			st := o.State()
			return "resume.log:\n" + tailLines(filepath.Join(o.Root(), "resume.log")) +
				"\nbuild.log (" + st.Project + "):\n" + tailLines(iorb.ImageLogPath(home, st.Project)), nil
		case "stop":
			if running := jobs(); len(running) > 0 && strings.TrimSpace(rest) != "force" {
				names := make([]string, len(running))
				for i, j := range running {
					names[i] = j.Cmd
				}
				return fmt.Sprintf("%d running job(s) would stop too: %s. Run /orb stop force to stop anyway.", len(running), strings.Join(names, ", ")), nil
			}
			ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
			defer cancel()
			if err := o.Stop(ctx); err != nil {
				return "", err
			}
			return "orb stopped; the next command starts it again", nil
		}
		return "", commands.SubmitAction(strings.TrimSpace("/orb " + args))
	}
}

// orbStatus is the phase line, then each step with its time.
func orbStatus(st iorb.State, now time.Time) string {
	var b strings.Builder
	b.WriteString(iorb.PhaseLine(st, now) + "\n")
	for _, ph := range st.Phases {
		end, mark := ph.EndedAt, "✓"
		switch {
		case ph.Error != "":
			mark = "✗"
		case end.IsZero():
			end, mark = now, "…"
		}
		took := end.Sub(ph.StartedAt).Round(100 * time.Millisecond).String()
		if ph.Name == iorb.PhaseReady {
			took = ""
		}
		fmt.Fprintf(&b, "  %s %-15s %6s", mark, iorb.PhaseWord(ph.Name), took)
		if ph.Error != "" {
			fmt.Fprintf(&b, "  %s", ph.Error)
		}
		b.WriteString("\n")
	}
	fmt.Fprintf(&b, "container %s\n", st.Container)
	if st.IP != "" && st.Status == iorb.StatusRunning {
		fmt.Fprintf(&b, "ip %s\n", st.IP)
	}
	for _, p := range st.Ports {
		if p.Error != "" {
			fmt.Fprintf(&b, "port %d not forwarded: %s\n", p.Guest, p.Error)
		} else {
			fmt.Fprintf(&b, "port 127.0.0.1:%d → %d\n", p.Host, p.Guest)
		}
	}
	fmt.Fprintf(&b, "image %s\nworktree %s", st.Image, st.Primary)
	return b.String()
}

func tailLines(path string) string {
	b, err := os.ReadFile(path)
	if err != nil {
		return "  (none)\n"
	}
	lines := strings.Split(strings.TrimRight(string(b), "\n"), "\n")
	if len(lines) > logTail {
		lines = lines[len(lines)-logTail:]
	}
	return strings.Join(lines, "\n") + "\n"
}
