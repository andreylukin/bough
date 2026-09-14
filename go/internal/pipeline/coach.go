package pipeline

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
)

const (
	coachEvery    = 90 * time.Second
	coachCooldown = 5 * time.Minute
	coachTail     = 40
	coachChars    = 8000
	coachMaxReply = 500 // runes; a longer reply is not a steering sentence
	coachTurnWait = 5 * time.Minute
)

// coach watches one agent node while its visit is in flight and sends
// it at most MaxSteers one-line steers. It never touches routing or
// status: every failure is a coach event and nothing more.
func (r *Runner) coach(ctx context.Context, c Coach, target func() (id string, running bool)) {
	every, cooldown := c.Every, c.Cooldown
	if every <= 0 {
		every = coachEvery
	}
	if cooldown <= 0 {
		cooldown = coachCooldown
	}
	s := r.opt.Sessions
	// One coach session for the whole run: a project-mode coach per tick
	// started a container every tick, and none was ever stopped.
	var sess string
	defer func() {
		if sess != "" {
			_ = s.Kill(sess)
		}
	}()
	var lastSeq int64
	var lastSteer time.Time
	steers, n := 0, 0
	tick := time.NewTicker(every)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
		}
		id, running := target()
		if !running || id == "" || !s.Live(id) || s.PendingAsk(id) != nil {
			continue
		}
		if c.MaxSteers > 0 && steers >= c.MaxSteers {
			continue
		}
		if !lastSteer.IsZero() && time.Since(lastSteer) < cooldown {
			continue
		}
		entries, err := s.Entries(id)
		if err != nil {
			r.event(Event{Kind: "coach", Node: c.Target, Text: c.Name + ": " + err.Error()})
			continue
		}
		var top int64
		for _, e := range entries {
			top = max(top, e.Seq)
		}
		if top <= lastSeq {
			continue
		}
		tail := render(serve.Transcript(entries, lastSeq, coachTail))
		lastSeq = top
		visit := r.inflight(c.Target)

		reply, err := r.askCoach(ctx, c, &sess, tail)
		n++
		verdict, why := "skipped", ""
		reply = strings.TrimSpace(reply)
		switch {
		case err != nil:
			why = err.Error()
		case reply == "" || reply == "NONE":
			why = "NONE"
		case utf8.RuneCountInString(reply) > coachMaxReply:
			why = "reply longer than 500 runes"
		default:
			// The turn may have ended while the coach thought: a steer
			// then would start a new turn instead of steering this one.
			// A coach session can read the host, holdout included.
			if hit, _ := leaks(reply, r.holdout); len(r.holdout) > 0 && hit {
				why = "leak"
			} else if err := r.steer(c.Target, id, visit, "[coach] "+reply); err != nil {
				why = err.Error()
			} else {
				verdict = "sent"
				steers++
				lastSteer = time.Now()
			}
		}
		body := fmt.Sprintf("# coach %s -> %s\n\n## Recent activity\n\n%s\n\n## Reply\n\n%s\n\n## %s", c.Name, c.Target, tail, reply, verdict)
		if why != "" {
			body += ": " + why
		}
		dir := filepath.Join(r.dir, "coach")
		_ = os.MkdirAll(dir, 0o755)
		_ = os.WriteFile(filepath.Join(dir, fmt.Sprintf("%d.md", n)), []byte(body+"\n"), 0o644)
		if verdict == "sent" {
			r.event(Event{Kind: "steer", Node: c.Target, Text: reply, Data: map[string]any{"coach": c.Name}})
		} else {
			r.event(Event{Kind: "coach", Node: c.Target, Text: c.Name + ": skipped: " + why})
		}
	}
}

// steer sends text to the target only if its visit is still the one the
// coach watched. The check and the Send hold r.mu, which the visit's end
// also takes, so a steer cannot land on a finished turn.
func (r *Runner) steer(node, id string, visit int, text string) error {
	s := r.opt.Sessions
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.st.Sessions[node] != id || r.active[node] == 0 || r.active[node] != visit || !s.Live(id) || s.PendingAsk(id) != nil {
		return errors.New("visit ended")
	}
	return s.Send(id, text)
}

// askCoach asks the run's coach session, creating it on first use (or
// again if it died), and returns its reply. It runs in its target's mode
// and project, so it can read no more than the coder: a local coach could
// read the holdout off the host.
func (r *Runner) askCoach(ctx context.Context, c Coach, sess *string, tail string) (string, error) {
	s := r.opt.Sessions
	if *sess == "" || !s.Live(*sess) {
		if *sess != "" {
			_ = s.Kill(*sess)
		}
		id := history.NewID()
		t := r.p.Nodes[c.Target]
		if _, err := s.Create(serve.CreateOptions{ID: id, Cwd: r.p.Dir, Mode: t.Mode, Slug: t.Project, Args: r.childArgs(c.Model), Origin: "loop"}); err != nil {
			return "", err
		}
		*sess = id
	}
	tctx, cancel := context.WithTimeout(ctx, coachTurnWait)
	defer cancel()
	reply, _, end := r.turn(tctx, *sess, c.Prompt+"\n\nGoal: "+r.p.Goal+"\n\nRecent activity:\n"+tail, nil)
	if end != "done" {
		return "", fmt.Errorf("coach turn ended: %s", end)
	}
	return reply, nil
}

func render(lines []serve.Line) string {
	var b strings.Builder
	for _, l := range lines {
		b.WriteString(l.Kind + ": " + l.Text + "\n")
	}
	s := b.String()
	if len(s) > coachChars {
		s = s[len(s)-coachChars:]
	}
	return s
}
