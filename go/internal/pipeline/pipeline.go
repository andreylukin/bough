// Package pipeline runs `bough loop`: a graph of agent nodes (headless
// bough children) and check nodes (shell commands), routed by exit codes
// and a strict verdict line, never by an agent. See docs/loops.md.
//
// Named pipeline, not loop, so it is not confused with plugins/loop,
// the turn loop.
package pipeline

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
)

type Pipeline struct {
	Name     string           `yaml:"name"`
	Goal     string           `yaml:"goal"`
	Start    string           `yaml:"start"`
	MaxSteps int              `yaml:"max_steps"`
	Nodes    map[string]*Node `yaml:"nodes"`
	Coaches  []Coach          `yaml:"coaches"`
	Dir      string           `yaml:"-"` // dir of the pipeline file; holdout globs resolve here

	raw []byte // the file as read, frozen into the run dir
}

type Node struct {
	Name      string        `yaml:"-"`
	Type      string        `yaml:"type"` // agent | check
	Mode      string        `yaml:"mode"` // agent: local | project
	Project   string        `yaml:"project"`
	Model     string        `yaml:"model"`   // "plugin/model"; empty = child's bough.yml
	Session   string        `yaml:"session"` // resume | fresh (default fresh)
	Prompt    string        `yaml:"prompt"`
	Holdout   []string      `yaml:"holdout"`
	Verdict   bool          `yaml:"verdict"`
	Run       string        `yaml:"run"` // check
	Cwd       string        `yaml:"cwd"` // check or agent: agent node name (its worktree) or path
	Timeout   time.Duration `yaml:"timeout"`
	Next      string        `yaml:"next"`
	Pass      string        `yaml:"pass"`
	Fail      string        `yaml:"fail"`
	MaxVisits int           `yaml:"max_visits"`
}

type Coach struct {
	Name      string        `yaml:"name"`
	Target    string        `yaml:"target"`
	Model     string        `yaml:"model"`
	Every     time.Duration `yaml:"every"`
	Cooldown  time.Duration `yaml:"cooldown"`
	MaxSteers int           `yaml:"max_steers"`
	Prompt    string        `yaml:"prompt"`
}

// The reserved targets: done ends the run passed, fail ends it failed.
const (
	targetDone = "done"
	targetFail = "fail"
)

// Sessions is the part of *serve.Supervisor the runner uses; *serve.Supervisor satisfies it.
type Sessions interface {
	Create(opt serve.CreateOptions) (string, error)
	Send(id, text string) error
	Subscribe(id string) (<-chan serve.Event, func())
	Entries(id string) ([]history.Entry, error)
	Live(id string) bool
	PendingAsk(id string) *serve.Ask
	Kill(id string) error
}

// Load parses and validates a pipeline file.
func Load(path string) (*Pipeline, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("pipeline: %w", err)
	}
	var p Pipeline
	dec := yaml.NewDecoder(bytes.NewReader(b))
	dec.KnownFields(true) // a typo'd key is an error, not a silent default
	if err := dec.Decode(&p); err != nil && !errors.Is(err, io.EOF) {
		if strings.Contains(err.Error(), "time.Duration") {
			err = fmt.Errorf("%w (use a duration like 10m)", err)
		}
		return nil, fmt.Errorf("pipeline: %s: %w", path, err)
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("pipeline: %w", err)
	}
	p.Dir = filepath.Dir(abs)
	p.raw = b
	if err := p.Validate(); err != nil {
		return nil, fmt.Errorf("pipeline: %s: %w", path, err)
	}
	return &p, nil
}

// Validate names every problem at once, so a pipeline is fixed in one pass.
func (p *Pipeline) Validate() error {
	var errs []error
	bad := func(f string, a ...any) { errs = append(errs, fmt.Errorf(f, a...)) }
	target := func(from, field, to string) {
		if to == targetDone || to == targetFail {
			return
		}
		if _, ok := p.Nodes[to]; !ok {
			bad("%s: %s %q is not a node", from, field, to)
		}
	}
	if len(p.Nodes) == 0 {
		return errors.New("no nodes")
	}
	if p.Start == "" {
		bad("start is required")
	} else {
		target("pipeline", "start", p.Start)
	}
	holdout := false
	for _, n := range p.Nodes {
		if len(n.Holdout) > 0 {
			holdout = true
		}
	}
	names := make([]string, 0, len(p.Nodes))
	for name := range p.Nodes {
		names = append(names, name)
	}
	slices.Sort(names)
	for _, name := range names {
		n := p.Nodes[name]
		if n == nil {
			bad("%s: empty node", name)
			continue
		}
		n.Name = name
		if name == targetDone || name == targetFail {
			bad("%s: reserved node name", name)
		}
		if n.MaxVisits <= 0 {
			bad("%s: max_visits is required", name)
		}
		for field, to := range map[string]string{"next": n.Next, "pass": n.Pass, "fail": n.Fail} {
			if to != "" {
				target(name, field, to)
			}
		}
		switch n.Type {
		case "agent":
			switch n.Mode {
			case "", "local":
				if holdout && len(n.Holdout) == 0 {
					bad("%s: a local agent node reads the whole host, so with holdout files it must hold them or be mode: project", name)
				}
			case "project":
				if n.Project == "" {
					bad("%s: project is required for mode: project", name)
				}
			default:
				bad("%s: mode %q: want local or project", name, n.Mode)
			}
			switch n.Session {
			case "", "fresh", "resume":
			default:
				bad("%s: session %q: want resume or fresh", name, n.Session)
			}
			if n.Model != "" && !strings.Contains(n.Model, "/") {
				bad("%s: model %q: want plugin/model", name, n.Model)
			}
			if n.Verdict {
				if n.Pass == "" || n.Fail == "" {
					bad("%s: a verdict node needs pass and fail", name)
				}
			} else if n.Next == "" || n.Fail == "" {
				bad("%s: an agent node needs next and fail", name)
			}
		case "check":
			if n.Verdict {
				bad("%s: verdict is only for agent nodes", name)
			}
			if n.Run == "" {
				bad("%s: run is required", name)
			}
			if n.Pass == "" || n.Fail == "" {
				bad("%s: a check node needs pass and fail", name)
			}
			if len(n.Holdout) > 0 {
				bad("%s: holdout is only for agent nodes", name)
			}
		default:
			bad("%s: type %q: want agent or check", name, n.Type)
		}
	}
	for _, c := range p.Coaches {
		if n, ok := p.Nodes[c.Target]; !ok || n == nil || n.Type != "agent" {
			bad("coach %s: target %q is not an agent node", c.Name, c.Target)
		}
		if c.Model != "" && !strings.Contains(c.Model, "/") {
			bad("coach %s: model %q: want plugin/model", c.Name, c.Model)
		}
	}
	return errors.Join(errs...)
}

// modelArgs turns "plugin/model" into the child's --set flags.
func modelArgs(model string) []string {
	plugin, name, ok := strings.Cut(model, "/")
	if !ok {
		return nil
	}
	return []string{"--set", "llm.plugin=" + plugin, "--set", "llm.model=" + name}
}
