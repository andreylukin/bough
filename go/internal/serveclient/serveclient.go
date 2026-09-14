// Package serveclient is the slice of the `bough serve` JSON API a
// session uses to start and watch background agents. Pure net/http, so
// a plugin can reach serve without importing it.
package serveclient

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"

	"github.com/andreylukin/bough/internal/servepid"
)

var ErrNoServe = errors.New("serveclient: no running bough serve")

// Addr reads $HOME/.bough/serve.pid, checks the pid is alive, and
// returns "http://<addr>". It never removes a stale file: that is
// cmd/bough's job, and a reader deleting it could race a serve that is
// just writing it.
func Addr(home string) (string, error) {
	b, err := os.ReadFile(filepath.Join(home, ".bough", "serve.pid"))
	if err != nil {
		return "", ErrNoServe
	}
	pid, addr, _, _, _, err := servepid.Parse(string(b))
	if err != nil || !servepid.Alive(pid) {
		return "", ErrNoServe
	}
	return "http://" + addr, nil
}

// Client talks to one serve. Parent is the calling session: serve only
// shows a session its own children.
type Client struct {
	Base   string
	HTTP   *http.Client
	Parent string
}

type ChildRequest struct {
	Cwd           string `json:"cwd,omitempty"`
	Prompt        string `json:"prompt"`
	Mode          string `json:"mode,omitempty"`
	Slug          string `json:"slug,omitempty"`
	SpawnedBy     string `json:"spawnedBy"`
	MaxPerSession int    `json:"maxPerSession,omitempty"`
	MaxRunning    int    `json:"maxRunning,omitempty"`
}

type ChildResponse struct {
	Session string
	Queued  bool
}

type AgentState struct {
	Status    string `json:"status"`
	Title     string `json:"title"`
	Reply     string `json:"reply"`
	Project   string `json:"project"`
	SpawnedBy string `json:"spawnedBy"`
}

// APIError is a non-2xx answer; Code lets a caller word its own message
// for the cases a model can act on (limit, depth, ownership).
type APIError struct {
	Code int
	Msg  string
}

func (e *APIError) Error() string { return e.Msg }

func (c *Client) CreateChild(ctx context.Context, req ChildRequest) (ChildResponse, error) {
	var out struct {
		Session struct {
			ID string `json:"id"`
		} `json:"session"`
		Queued bool `json:"queued"`
	}
	if err := c.do(ctx, http.MethodPost, "/api/sessions", req, &out); err != nil {
		return ChildResponse{}, err
	}
	return ChildResponse{Session: out.Session.ID, Queued: out.Queued}, nil
}

func (c *Client) Agent(ctx context.Context, id string) (AgentState, error) {
	var out AgentState
	err := c.do(ctx, http.MethodGet, "/api/sessions/"+url.PathEscape(id)+"/agent?parent="+url.QueryEscape(c.Parent), nil, &out)
	return out, err
}

// Stop interrupts a running child or drops a queued one; was is
// "running", "queued" or "idle".
func (c *Client) Stop(ctx context.Context, id string) (string, error) {
	var out struct {
		Was string `json:"was"`
	}
	err := c.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/stop", map[string]string{"parent": c.Parent}, &out)
	return out.Was, err
}

func (c *Client) Interrupt(ctx context.Context, id string) error {
	_, err := c.Stop(ctx, id)
	return err
}

func (c *Client) do(ctx context.Context, method, path string, body, out any) error {
	var rdr io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return fmt.Errorf("serveclient: encode: %w", err)
		}
		rdr = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.Base+path, rdr)
	if err != nil {
		return fmt.Errorf("serveclient: %w", err)
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	hc := c.HTTP
	if hc == nil {
		hc = http.DefaultClient
	}
	resp, err := hc.Do(req)
	if err != nil {
		return fmt.Errorf("serveclient: %s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		var e struct {
			Error string `json:"error"`
		}
		msg := string(bytes.TrimSpace(raw))
		if json.Unmarshal(raw, &e) == nil && e.Error != "" {
			msg = e.Error
		}
		return &APIError{Code: resp.StatusCode, Msg: msg}
	}
	if out != nil {
		if err := json.Unmarshal(raw, out); err != nil {
			return fmt.Errorf("serveclient: decode %s: %w", path, err)
		}
	}
	return nil
}
