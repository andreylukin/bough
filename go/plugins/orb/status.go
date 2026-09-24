package orb

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/tools"
)

const logTail = 40

// orbLike is what /orb needs of the session's orb: the handle, which
// answers before the container is up.
type orbLike interface {
	State() iorb.State
	Root() string
	Stop(context.Context) error
	Restart(fresh bool, by string) string
}

// orbCommand is /orb in a project session: status, logs, stop and
// restart, while any other argument still runs the /orb setup skill.
func orbCommand(o orbLike, home string, jobs func() []tools.Running) func(string) (string, error) {
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
		case "restart":
			// Never refused the way stop is: the swap waits for the turn,
			// and the notice says which jobs stopped.
			fresh := false
			switch strings.TrimSpace(rest) {
			case "":
			case "fresh":
				fresh = true
			default:
				return "", fmt.Errorf("usage: /orb restart [fresh]")
			}
			text := o.Restart(fresh, "person")
			if running := jobs(); len(running) > 0 {
				names := make([]string, len(running))
				for i, j := range running {
					names[i] = j.Cmd
				}
				text += fmt.Sprintf(" %d running job(s) will stop: %s.", len(running), strings.Join(names, ", "))
			}
			return text, nil
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
