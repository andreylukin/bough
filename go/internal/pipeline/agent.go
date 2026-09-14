package pipeline

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
)

// askPoll is how often an agent visit looks for a pending ask: loop
// nodes are unattended, so one fails the visit.
var askPoll = 2 * time.Second

// closeWait bounds how long a visit waits for the closing entry on disk
// after the done event.
const closeWait = 2 * time.Second

// childArgs is every --set for a child: the run's, then the model's.
func (r *Runner) childArgs(model string) []string {
	var args []string
	for _, s := range r.opt.Sets {
		args = append(args, "--set", s)
	}
	return append(args, modelArgs(model)...)
}

// newSession creates a loop child under a pre-minted id and pins its
// model with an idempotent /model line, so a respawn without the args
// still runs the same model.
func (r *Runner) newSession(cwd, mode, slug, model string) (string, error) {
	id := history.NewID()
	if _, err := r.opt.Sessions.Create(serve.CreateOptions{ID: id, Cwd: cwd, Mode: mode, Slug: slug, Args: r.childArgs(model), Origin: "loop"}); err != nil {
		return "", err
	}
	if plugin, name, ok := strings.Cut(model, "/"); ok {
		if err := r.opt.Sessions.Send(id, "/model "+plugin+" "+name); err != nil {
			_ = r.opt.Sessions.Kill(id)
			return "", err
		}
	}
	return id, nil
}

// turn sends one prompt and waits for the turn to end. end is the event
// kind that ended it (done, cancelled, exit), "ask" or "stopped".
// ended, if set, runs as soon as the turn is over, before the wait for
// the closing entry, so a coach stops steering a finished turn.
func (r *Runner) turn(ctx context.Context, id, prompt string, ended func()) (reply string, errored bool, end string) {
	s := r.opt.Sessions
	ch, unsub := s.Subscribe(id)
	defer unsub()
	if err := s.Send(id, prompt); err != nil {
		return err.Error(), true, "exit"
	}
	tick := time.NewTicker(askPoll)
	defer tick.Stop()
wait:
	for {
		select {
		case <-ctx.Done():
			_ = s.Kill(id)
			return "", true, "stopped"
		case <-tick.C:
			if s.PendingAsk(id) != nil {
				_ = s.Kill(id)
				return "", true, "ask"
			}
		case ev, ok := <-ch:
			if !ok {
				end = "exit"
				break wait
			}
			switch ev.Kind {
			case "done", "cancelled", "exit":
				end = ev.Kind
				break wait
			}
		}
	}
	if ended != nil {
		ended()
	}
	deadline := time.Now().Add(closeWait)
	var entries []history.Entry
	for {
		entries, _ = s.Entries(id)
		if closed(entries) || time.Now().After(deadline) {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	reply, errored = serve.LastTurn(entries)
	return reply, errored, end
}

// closed is whether the last turn on disk has its done/cancelled entry.
func closed(entries []history.Entry) bool {
	for i := len(entries) - 1; i >= 0; i-- {
		switch entries[i].Kind {
		case "done", "cancelled":
			return true
		case "input":
			return false
		}
	}
	return false
}

// agent runs one agent visit and fills res; the returned text is what
// the next node receives as {{input}}.
func (r *Runner) agent(ctx context.Context, n *Node, prompt, sdir string, res *result) (routed string) {
	r.mu.Lock()
	id := r.st.Sessions[n.Name]
	r.mu.Unlock()
	if n.Session != "resume" || id == "" {
		// cwd: <node> starts a validator in the coder's worktree; without
		// it a local node read the pipeline's original repo, not the edits.
		cwd, err := r.checkDir(n)
		if err != nil {
			res.Route, res.Reason = n.Fail, "cwd: "+err.Error()
			return res.Reason
		}
		if id, err = r.newSession(cwd, n.Mode, n.Project, n.Model); err != nil {
			res.Route, res.Reason = n.Fail, "create: "+err.Error()
			return res.Reason
		}
		// A fresh visit replaces the previous child; kill it so the run
		// holds one live child per node.
		r.mu.Lock()
		prev := r.st.Sessions[n.Name]
		r.st.Sessions[n.Name] = id
		r.mu.Unlock()
		if prev != "" && r.opt.Sessions.Live(prev) {
			_ = r.opt.Sessions.Kill(prev)
		}
		r.saveState()
	}
	res.Session = id
	_ = os.WriteFile(filepath.Join(sdir, "prompt.md"), []byte(prompt), 0o644)
	r.event(Event{Kind: "prompt", Node: n.Name, Step: res.step, Text: prompt, Data: map[string]any{"session": id}})

	r.setInflight(n.Name, res.step)
	reply, errored, end := r.turn(ctx, id, prompt, func() { r.setInflight(n.Name, 0) })
	r.setInflight(n.Name, 0)

	_ = os.WriteFile(filepath.Join(sdir, "reply.md"), []byte(reply), 0o644)
	r.event(Event{Kind: "reply", Node: n.Name, Step: res.step, Text: reply, Data: map[string]any{"end": end, "errored": errored}})

	switch {
	case end == "stopped":
		res.Reason = "stopped"
	case end == "ask":
		res.Route, res.Reason = n.Fail, "ask"
	case n.Verdict:
		pass, ok := verdictOf(reply)
		res.Verdict = map[bool]string{true: "PASS", false: "FAIL"}[pass]
		switch {
		case end != "done":
			res.Route, res.Reason = n.Fail, end
		case !ok:
			res.Route, res.Reason, res.Verdict = n.Fail, "no verdict line", "FAIL"
		case pass:
			res.Route, res.Reason = n.Pass, "VERDICT: PASS"
		default:
			res.Route, res.Reason = n.Fail, "VERDICT: FAIL"
		}
	case end == "done" && !errored:
		res.Route, res.Reason = n.Next, "done"
	case end == "done":
		res.Route, res.Reason = n.Fail, "errored"
	default:
		res.Route, res.Reason = n.Fail, end
	}

	routed = reply
	if len(n.Holdout) > 0 {
		if hit, phrase := leaks(reply, r.holdout); hit {
			routed = withheld
			r.event(Event{Kind: "leak", Node: n.Name, Step: res.step, Text: phrase})
		}
	}
	_ = os.WriteFile(filepath.Join(sdir, "routed.md"), []byte(routed), 0o644)
	return routed
}
