package workers

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/serveclient"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

const (
	defaultMaxPerSession = 200
	defaultMaxRunning    = 16
	// serveTimeout bounds one API call: serve answers a create before the
	// child boots, so anything slower is a hung serve, not a slow agent.
	serveTimeout = 30 * time.Second
)

// errNoServe is the exact text the model reads when serve is not up.
var errNoServe = errors.New("background agents need bough serve (start it with `bough serve`)")

// spawnBackground is tools.spawn(task, {background: true, project?, model?}).
// It never touches the blocking spawn's per-turn budget or depth flag:
// the child is a separate process with its own turn.
func (w *Workers) spawnBackground(task string, opts map[string]any) (any, error) {
	for k := range opts {
		if k != "background" && k != "project" && k != "model" {
			return nil, fmt.Errorf("workers: a background agent reports text; drop the schema")
		}
	}
	if bg, _ := opts["background"].(bool); !bg {
		// {background: false} is a plain blocking spawn with no schema.
		return w.spawn(task)
	}
	slug, _ := opts["project"].(string)
	model, _ := opts["model"].(string)
	m, err := w.startAgent(w.turnCtx(), task, slug, model)
	if err != nil {
		return nil, err
	}
	return m, nil
}

// startAgent asks serve for a background agent under parent (the
// block's turn, or a native call's context).
func (w *Workers) startAgent(parent context.Context, task, slug, model string) (map[string]any, error) {
	if strings.TrimSpace(task) == "" {
		return nil, fmt.Errorf("workers: spawn needs a non-empty task")
	}
	if w.spawnedBy() != "" {
		return nil, fmt.Errorf("workers: a background agent cannot start agents (depth 1)")
	}
	// The child runs on this session's model unless told otherwise: the
	// config default may be a provider this person has no key for.
	if model == "" {
		model = w.currentModel()
	}
	c, err := w.client()
	if err != nil {
		return nil, err
	}
	cwd, _ := os.Getwd()
	ctx, cancel := context.WithTimeout(parent, serveTimeout)
	defer cancel()
	resp, err := c.CreateChild(ctx, serveclient.ChildRequest{
		Cwd: cwd, Prompt: task, Slug: slug, Model: model, SpawnedBy: c.Parent,
		MaxPerSession: w.maxPerSession, MaxRunning: w.maxRunning,
	})
	if err != nil {
		var ae *serveclient.APIError
		if errors.As(err, &ae) {
			switch {
			case ae.Code == http.StatusTooManyRequests:
				return nil, fmt.Errorf("workers: background agent limit reached (%d per session) — do the remaining work yourself", w.maxPerSession)
			case ae.Code == http.StatusConflict:
				return nil, fmt.Errorf("workers: a background agent cannot start agents (depth 1)")
			case ae.Code == http.StatusBadRequest && strings.Contains(ae.Msg, "unknown project"):
				return nil, fmt.Errorf("workers: unknown project %q", slug)
			}
		}
		return nil, fmt.Errorf("workers: background spawn: %w", err)
	}
	status := "running"
	if resp.Queued {
		status = "queued"
	}
	return map[string]any{"session": resp.Session, "status": status}, nil
}

// currentModel is this session's llm row as "plugin/model", read per
// call since /model swaps the row mid-session; "" when unknown.
func (w *Workers) currentModel() string {
	if w.kctx == nil {
		return ""
	}
	for _, r := range w.kctx.Desired() {
		if r.ID != "llm" {
			continue
		}
		if m, _ := r.Config["model"].(string); m != "" && r.Plugin != "" {
			return r.Plugin + "/" + m
		}
	}
	return ""
}

// agent is tools.agent(id).
func (w *Workers) agent(id string) (map[string]any, error) {
	return w.agentIn(w.turnCtx(), id)
}

func (w *Workers) agentIn(parent context.Context, id string) (map[string]any, error) {
	c, err := w.client()
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithTimeout(parent, serveTimeout)
	defer cancel()
	st, err := c.Agent(ctx, id)
	if err != nil {
		return nil, agentErr(id, err)
	}
	return map[string]any{"status": st.Status, "title": st.Title, "reply": st.Reply, "project": st.Project}, nil
}

// stopAgent is tools.stopAgent(id).
func (w *Workers) stopAgent(id string) (string, error) {
	return w.stopAgentIn(w.turnCtx(), id)
}

func (w *Workers) stopAgentIn(parent context.Context, id string) (string, error) {
	c, err := w.client()
	if err != nil {
		return "", err
	}
	ctx, cancel := context.WithTimeout(parent, serveTimeout)
	defer cancel()
	was, err := c.Stop(ctx, id)
	if err != nil {
		return "", agentErr(id, err)
	}
	if was == "idle" {
		return "not running", nil
	}
	return "stopped", nil
}

func agentErr(id string, err error) error {
	var ae *serveclient.APIError
	if errors.As(err, &ae) {
		switch ae.Code {
		case http.StatusNotFound:
			return fmt.Errorf("workers: no agent %q", id)
		case http.StatusForbidden:
			return fmt.Errorf("workers: agent %q was not started by this session", id)
		}
	}
	return fmt.Errorf("workers: agent %q: %w", id, err)
}

// client finds serve and names this session as the parent. Resolved per
// call: serve may start or stop while the session runs.
func (w *Workers) client() (*serveclient.Client, error) {
	base, err := serveclient.Addr(w.home)
	if err != nil {
		return nil, errNoServe
	}
	parent := w.sessionID()
	if parent == "" {
		return nil, fmt.Errorf("workers: background spawn: this session has no history file to be a parent")
	}
	c := &serveclient.Client{Base: base, Parent: parent}
	if w.httpClient != nil {
		c.HTTP = w.httpClient()
	}
	return c, nil
}

// sessionID is this session's id: its history file's basename.
func (w *Workers) sessionID() string {
	if w.kctx == nil {
		return ""
	}
	p, err := kernel.Get[interface{ Path() string }](w.kctx, "history")
	if err != nil || p.Path() == "" {
		return ""
	}
	return strings.TrimSuffix(filepath.Base(p.Path()), ".jsonl")
}

// spawnedBy is this session's parent: the launcher's value for a fresh
// child, else the meta entry of a resumed one (serve restarts a child
// without the env).
func (w *Workers) spawnedBy() string {
	if w.kctx == nil {
		return ""
	}
	if v, err := kernel.Get[string](w.kctx, "session-spawned-by"); err == nil && v != "" {
		return v
	}
	h, err := kernel.Get[interface{ Entries() []history.Entry }](w.kctx, "history")
	if err != nil {
		return ""
	}
	for _, e := range h.Entries() {
		if e.Kind == "meta" {
			if v, _ := e.Data["spawned_by"].(string); v != "" {
				return v
			}
		}
	}
	return ""
}
