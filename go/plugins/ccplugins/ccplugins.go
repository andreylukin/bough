// Package ccplugins reads Claude Code's installed-plugin manifest
// (~/.claude/plugins/installed_plugins.json) so bough can pick up the
// plugins the user already has: their skills/ directories join the
// skills pools, their commands/ join the command registry.
//
// Two things are deliberately loud rather than silent. A manifest
// entry can name an installPath that is not on disk — the plugin is
// listed as installed but absent from the cache — and that entry is
// still reported, with Present false; a plugin that quietly fails to
// load is worse than one that errors. And hooks/hooks.json is written
// against Claude Code's event names, only some of which mean anything
// here: the rest are reported as ignored rather than dropped.
package ccplugins

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/offlist"
)

// Plugin is one manifest entry, as the API reports it. Its ID —
// "<name>@<marketplace>" — is what off.yml lists under "plugin:".
type Plugin struct {
	ID          string   `json:"id"`
	Name        string   `json:"name"`
	Marketplace string   `json:"marketplace"`
	Version     string   `json:"version"`
	Scope       string   `json:"scope"`       // "user" or "project"
	ProjectPath string   `json:"projectPath"` // project scope only
	InstallPath string   `json:"installPath"`
	Present     bool     `json:"present"` // installPath exists on disk
	Skills      []string `json:"skills"`
	Commands    []string `json:"commands"`
	Off         bool     `json:"off"`
}

// Path is the manifest for a given user home.
func Path(home string) string {
	return filepath.Join(home, ".claude", "plugins", "installed_plugins.json")
}

// manifest is the on-disk shape (version 2): id -> installations.
type manifest struct {
	Version int                      `json:"version"`
	Plugins map[string][]installment `json:"plugins"`
}

type installment struct {
	Scope       string `json:"scope"`
	ProjectPath string `json:"projectPath"`
	InstallPath string `json:"installPath"`
	Version     string `json:"version"`
}

// Load reads the manifest and returns every entry in it, sorted by id.
// A missing manifest is the normal case and yields no plugins and no
// error; a malformed one is an error, not an empty install list, so
// the caller can say so.
func Load(home string) ([]Plugin, error) {
	data, err := os.ReadFile(Path(home))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, err
	}
	var m manifest
	if err := json.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("%s: %w", Path(home), err)
	}

	off := offlist.Load(filepath.Join(home, ".bough"))
	var out []Plugin
	for id, installs := range m.Plugins {
		name, market, _ := strings.Cut(id, "@")
		for _, in := range installs {
			p := Plugin{
				ID: id, Name: name, Marketplace: market,
				Version:     in.Version,
				Scope:       in.Scope,
				ProjectPath: in.ProjectPath,
				InstallPath: in.InstallPath,
				Skills:      []string{},
				Commands:    []string{},
				Off:         off.Off("plugin", id),
			}
			if p.Scope == "" {
				p.Scope = "user"
			}
			if st, err := os.Stat(in.InstallPath); err == nil && st.IsDir() {
				p.Present = true
				p.Skills = dirNames(filepath.Join(in.InstallPath, "skills"), true)
				p.Commands = commandNames(filepath.Join(in.InstallPath, "commands"))
			}
			out = append(out, p)
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].ID != out[j].ID {
			return out[i].ID < out[j].ID
		}
		return out[i].InstallPath < out[j].InstallPath
	})
	return out, nil
}

// Installed is Load for callers with nowhere to put an error: the
// manifest's own trouble goes to the log, and a plugin listed but
// missing from the cache is named there too — it is invisible in the
// pools otherwise.
func Installed(home string) []Plugin {
	plugins, err := Load(home)
	if err != nil {
		kernel.Logf("ccplugins: %v\n", err)
		return nil
	}
	for _, p := range plugins {
		if !p.Present {
			kernel.Logf("ccplugins: %s installed but not present at %s — skipped\n", p.ID, p.InstallPath)
		}
	}
	return plugins
}

// Active returns the plugins that apply while working under workPath:
// present, not switched off, and — for a project-scope plugin — only
// when workPath is inside its projectPath. The user runs bough from
// home, so the path being worked on decides this, never the process's
// own working directory.
func Active(home, workPath string) []Plugin {
	var out []Plugin
	for _, p := range Installed(home) {
		if !p.Present || p.Off {
			continue
		}
		if p.Scope == "project" && !inside(workPath, p.ProjectPath) {
			continue
		}
		out = append(out, p)
	}
	return out
}

// SkillDirs are the skills/ directories of the active plugins, in id
// order, for appending to the skills pools.
func SkillDirs(home, workPath string) []string {
	return subdirs(Active(home, workPath), "skills")
}

// CommandDirs are the commands/ directories of the active plugins.
func CommandDirs(home, workPath string) []string {
	return subdirs(Active(home, workPath), "commands")
}

func subdirs(plugins []Plugin, name string) []string {
	var out []string
	for _, p := range plugins {
		d := filepath.Join(p.InstallPath, name)
		if st, err := os.Stat(d); err == nil && st.IsDir() {
			out = append(out, d)
		}
	}
	return out
}

// inside reports whether path is root or lives under it.
func inside(path, root string) bool {
	if root == "" || path == "" {
		return false
	}
	path, root = filepath.Clean(path), filepath.Clean(root)
	if path == root {
		return true
	}
	return strings.HasPrefix(path, root+string(filepath.Separator))
}

// dirNames lists the entries of dir (directories only when dirsOnly),
// sorted; a missing dir yields none.
func dirNames(dir string, dirsOnly bool) []string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return []string{}
	}
	out := []string{}
	for _, e := range entries {
		if dirsOnly && !e.IsDir() {
			continue
		}
		out = append(out, e.Name())
	}
	slices.Sort(out)
	return out
}

// commandNames lists the "*.md" command files of dir by their command
// name (no extension).
func commandNames(dir string) []string {
	out := []string{}
	for _, n := range dirNames(dir, false) {
		if name, ok := strings.CutSuffix(n, ".md"); ok {
			out = append(out, name)
		}
	}
	return out
}

// events maps Claude Code's hook events onto bough's. Only the four
// that genuinely correspond are here: the rest of Claude Code's
// vocabulary (Notification, Stop, SubagentStop, PreCompact,
// UserPromptSubmit, …) has no bough event that would fire at the same
// moment, and guessing one would run the user's command at the wrong
// time.
var events = map[string]string{
	"SessionStart": "session-start",
	"SessionEnd":   "session-end",
	"PreToolUse":   "pre-code-exec",
	"PostToolUse":  "post-result",
}

// Hook is one command a plugin wants run on a bough event.
type Hook struct {
	Plugin  string `json:"plugin"`  // plugin id
	Event   string `json:"event"`   // bough event
	CCEvent string `json:"ccEvent"` // as written in hooks.json
	Matcher string `json:"matcher"`
	Command string `json:"command"`
}

// hooksFile is Claude Code's hooks.json: event -> matcher groups.
// Some plugins wrap the event map in a "hooks" key, some do not.
type hooksFile struct {
	Hooks map[string][]hookGroup `json:"hooks"`
}

type hookGroup struct {
	Matcher string `json:"matcher"`
	Hooks   []struct {
		Type    string `json:"type"`
		Command string `json:"command"`
	} `json:"hooks"`
}

// Hooks reads a plugin's hooks/hooks.json and returns the hooks whose
// event maps onto a bough one, plus the Claude Code event names it
// skipped — reported, never pretended to have loaded.
func Hooks(p Plugin) (hooks []Hook, ignored []string, err error) {
	path := filepath.Join(p.InstallPath, "hooks", "hooks.json")
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil, nil
		}
		return nil, nil, err
	}

	var f hooksFile
	if err := json.Unmarshal(data, &f); err != nil {
		return nil, nil, fmt.Errorf("%s: %w", path, err)
	}
	byEvent := f.Hooks
	if byEvent == nil {
		// A file written as a bare event map, with no "hooks" wrapper.
		if err := json.Unmarshal(data, &byEvent); err != nil {
			return nil, nil, fmt.Errorf("%s: %w", path, err)
		}
	}

	for _, cc := range slices.Sorted(mapKeys(byEvent)) {
		ev, ok := events[cc]
		if !ok {
			ignored = append(ignored, cc)
			continue
		}
		for _, g := range byEvent[cc] {
			for _, h := range g.Hooks {
				if h.Command == "" {
					continue
				}
				hooks = append(hooks, Hook{Plugin: p.ID, Event: ev, CCEvent: cc,
					Matcher: g.Matcher, Command: h.Command})
			}
		}
	}
	return hooks, ignored, nil
}

func mapKeys[V any](m map[string]V) func(func(string) bool) {
	return func(yield func(string) bool) {
		for k := range m {
			if !yield(k) {
				return
			}
		}
	}
}
