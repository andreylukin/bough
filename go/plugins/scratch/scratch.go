// Package scratch is the "scratchpad" plugin: a place for the working
// notes and temporary files an agent produces, and for the values it
// needs to carry from one code block to the next.
//
// It answers two problems bough has by construction. The first is
// mess: a session that needs a throwaway script, a probe, a copy of a
// file to diff against, writes it into the user's checkout — today's
// runs left probe.go, rewrite.go and dx.txt behind. The second is
// forgetting: each code block runs in its own function scope, so a
// value one block computes is gone in the next unless it was printed
// (and printing it spends context on every later call). A scratchpad
// is the fix both times — the filesystem as memory, outside the tree.
//
// Everything lives under ~/.bough/scratch/<session>/: named values in
// state.json, free-form notes in notes.md, and whatever files the
// model or its shell commands write. $BOUGH_SCRATCH points there, so
// tools.bash can use it without knowing anything about this plugin.
package scratch

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

// stateFile holds the named values; notesFile the running notes.
const (
	stateFile = "state.json"
	notesFile = "notes.md"
)

// maxValue bounds one stored value: a scratchpad is for notes and
// intermediate results, not for parking a repository in memory.
const maxValue = 1 << 20

// registry is the slice of codemode this plugin needs.
type registry interface {
	RegisterTool(name string, fn any)
}

// describer is codemode's prompt-catalogue seam.
type describer interface{ Describe(name, line string) }

// sections is the loop's prompt-sections registry.
type sections interface{ Set(name, text string) }

// pather is the history service's seam: the session file names the
// scratchpad, so a resumed session finds its own notes again.
type pather interface{ Path() string }

// Pad is the scratchpad: a directory, and the named values in it.
type Pad struct {
	dir string

	mu     sync.Mutex
	values map[string]any
}

// New opens (and creates) a scratchpad at dir, reading back whatever
// an earlier session in the same directory left.
func New(dir string) (*Pad, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("scratch: %w", err)
	}
	p := &Pad{dir: dir, values: map[string]any{}}
	if b, err := os.ReadFile(filepath.Join(dir, stateFile)); err == nil {
		_ = json.Unmarshal(b, &p.values) // a corrupt state file starts empty
	}
	return p, nil
}

// Dir is the scratchpad's path.
func (p *Pad) Dir() string { return p.dir }

// Set stores a value under name for the rest of the session (and the
// next one, resumed). Returns what a model wants to see: that it
// landed, and how big it is.
func (p *Pad) Set(name string, v any) (string, error) {
	if strings.TrimSpace(name) == "" {
		return "", fmt.Errorf("scratch.set: a name is required")
	}
	b, err := json.Marshal(v)
	if err != nil {
		return "", fmt.Errorf("scratch.set %s: %w", name, err)
	}
	if len(b) > maxValue {
		return "", fmt.Errorf("scratch.set %s: %s is too big for a value — write it to a file with scratch.file(name)", name, size(len(b)))
	}
	p.mu.Lock()
	p.values[name] = v
	p.mu.Unlock()
	if err := p.save(); err != nil {
		return "", err
	}
	return fmt.Sprintf("scratch.set %s (%s)", name, size(len(b))), nil
}

// Get returns a stored value, or an error naming what IS stored: a
// model that mistypes a key should not have to guess twice.
func (p *Pad) Get(name string) (any, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if v, ok := p.values[name]; ok {
		return v, nil
	}
	if len(p.values) == 0 {
		return nil, fmt.Errorf("scratch.get %s: nothing stored yet", name)
	}
	return nil, fmt.Errorf("scratch.get %s: not stored — the pad holds: %s", name, strings.Join(p.names(), ", "))
}

// Keys is the stored names, sorted.
func (p *Pad) Keys() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.names()
}

// Drop removes a value.
func (p *Pad) Drop(name string) (string, error) {
	p.mu.Lock()
	_, had := p.values[name]
	delete(p.values, name)
	p.mu.Unlock()
	if !had {
		return "", fmt.Errorf("scratch.drop %s: not stored", name)
	}
	return "scratch.drop " + name, p.save()
}

// Note appends a line to notes.md with a timestamp: the durable half
// of a scratchpad, for findings that must survive the context window.
func (p *Pad) Note(text string) (string, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return "", fmt.Errorf("scratch.note: nothing to write")
	}
	path := filepath.Join(p.dir, notesFile)
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return "", fmt.Errorf("scratch.note: %w", err)
	}
	defer f.Close()
	if _, err := fmt.Fprintf(f, "- %s %s\n", time.Now().Format("15:04"), text); err != nil {
		return "", fmt.Errorf("scratch.note: %w", err)
	}
	return "noted in " + path, nil
}

// Notes is what has been noted so far, "" when nothing has.
func (p *Pad) Notes() string {
	b, err := os.ReadFile(filepath.Join(p.dir, notesFile))
	if err != nil {
		return ""
	}
	return string(b)
}

// File is an absolute path inside the scratchpad for name, with its
// parent made: where a throwaway script or a copy of a file goes,
// instead of the user's checkout.
func (p *Pad) File(name string) (string, error) {
	clean := filepath.Clean("/" + strings.TrimSpace(name))[1:]
	if clean == "" || clean == "." {
		return "", fmt.Errorf("scratch.file: a name is required")
	}
	full := filepath.Join(p.dir, clean)
	if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
		return "", fmt.Errorf("scratch.file: %w", err)
	}
	return full, nil
}

// List renders the scratchpad for a person: the values, the notes and
// the files, with sizes.
func (p *Pad) List() string {
	var b strings.Builder
	fmt.Fprintf(&b, "scratchpad: %s\n", p.dir)
	if keys := p.Keys(); len(keys) > 0 {
		fmt.Fprintf(&b, "values: %s\n", strings.Join(keys, ", "))
	}
	var files []string
	ents, _ := os.ReadDir(p.dir)
	for _, e := range ents {
		if e.Name() == stateFile {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		files = append(files, fmt.Sprintf("%s (%s)", e.Name(), size(int(info.Size()))))
	}
	if len(files) > 0 {
		fmt.Fprintf(&b, "files: %s\n", strings.Join(files, ", "))
	}
	if len(p.Keys()) == 0 && len(files) == 0 {
		b.WriteString("(empty)\n")
	}
	return strings.TrimRight(b.String(), "\n")
}

// names is Keys without the lock (callers hold it).
func (p *Pad) names() []string {
	out := make([]string, 0, len(p.values))
	for k := range p.values {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// save writes the values, atomically so a killed turn cannot leave a
// half-written state file behind.
func (p *Pad) save() error {
	p.mu.Lock()
	b, err := json.MarshalIndent(p.values, "", " ")
	p.mu.Unlock()
	if err != nil {
		return fmt.Errorf("scratch: %w", err)
	}
	tmp := filepath.Join(p.dir, stateFile+".tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return fmt.Errorf("scratch: %w", err)
	}
	return os.Rename(tmp, filepath.Join(p.dir, stateFile))
}

func size(n int) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1f MB", float64(n)/(1<<20))
	case n >= 1024:
		return fmt.Sprintf("%.1f kB", float64(n)/1024)
	}
	return fmt.Sprintf("%d B", n)
}

// PromptSection tells the model the scratchpad exists and what it is
// for. Values first: carrying a result between blocks is the thing
// the runtime cannot do on its own.
const PromptSection = `Scratchpad — your own directory and memory, at %s (also $BOUGH_SCRATCH in tools.bash):
- tools.scratch.set(name, value) / tools.scratch.get(name) carry a VALUE between code blocks. Declarations do not survive a block; this does, and it does not cost context the way printing does. Use it for the list of files you are working through, a parsed config, a count you will need later.
- tools.scratch.note(text) appends a line to notes.md: findings, decisions, dead ends. Write them down as you go — this conversation is finite and the file is not.
- tools.scratch.file(name) -> an absolute path in the scratchpad. EVERY throwaway file goes there: probe scripts, copies to diff against, downloaded archives. Never write a temporary file into the user's repository.
- tools.scratch.keys(), tools.scratch.notes(), tools.scratch.drop(name), tools.scratch.dir().`

type plugin struct{}

func init() {
	kernel.Register("scratchpad", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "scratchpad" }
func (plugin) Inject() []string { return []string{"codemode"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "dir" {
			return fmt.Errorf("scratchpad: unknown config key %q", k)
		}
	}
	dir, _ := cfg["dir"].(string)
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("scratchpad: home dir: %w", err)
		}
		// Named after the session, so a resumed session finds its own
		// notes and values again.
		name := "session"
		if h, err := kernel.Get[pather](ctx, "history"); err == nil && h.Path() != "" {
			name = strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
		}
		dir = filepath.Join(home, ".bough", "scratch", name)
	}
	pad, err := New(dir)
	if err != nil {
		return err
	}
	// tools.bash inherits it, so a shell command can use the
	// scratchpad without this plugin knowing about that one.
	if err := os.Setenv("BOUGH_SCRATCH", pad.Dir()); err != nil {
		return fmt.Errorf("scratchpad: %w", err)
	}

	code, err := kernel.Get[registry](ctx, "codemode")
	if err != nil {
		return err
	}
	code.RegisterTool("scratch", map[string]any{
		"set":   pad.Set,
		"get":   pad.Get,
		"keys":  pad.Keys,
		"drop":  pad.Drop,
		"note":  pad.Note,
		"notes": pad.Notes,
		"file":  pad.File,
		"dir":   pad.Dir,
		"list":  pad.List,
	})
	if d, ok := code.(describer); ok {
		d.Describe("scratch", `tools.scratch.set(name, value) / .get(name) carry a value between blocks; .note(text) writes a durable finding; .file(name) -> a path for a throwaway file (never write one into the repo); .keys() .notes() .drop() .dir() .list().`)
	}
	ctx.Effect(func() { code.RegisterTool("scratch", nil) })

	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set("scratch", fmt.Sprintf(PromptSection, pad.Dir()))
		ctx.Effect(func() { s.Set("scratch", "") })
	}
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "scratch", Summary: "show the scratchpad: values, notes and files"}
		if err := reg.Register(info, func(string) (string, error) { return pad.List(), nil }); err != nil {
			return err
		}
		ctx.Effect(func() { reg.Unregister("scratch") })
	}
	ctx.Provide("scratch", pad)
	return nil
}
