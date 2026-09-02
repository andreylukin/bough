// Package graphiti is the "graphiti" plugin: long-term memory for bough
// on getzep/graphiti, self-hosted with no Docker. One launchd job runs
// Graphiti's stock MCP server over an embedded FalkorDB (falkordblite,
// one file under ~/.bough/graphiti); every bough talks to it over http
// through the mcp row, and two hook files carry the memory loop:
//
//	user-prompt-submit  → search_memory_facts, appended as a [memory] block
//	stop                → add_memory of the turn, in the background
//
// `bough graphiti install` builds all of it; the mounted row only adds a
// prompt section telling the model the memory exists.
package graphiti

import (
	"bytes"
	"embed"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
)

//go:embed assets/*
var assets embed.FS

const (
	label    = "com.bough.graphiti"
	upstream = "https://github.com/getzep/graphiti.git"
)

type sections interface {
	Set(name, text string)
}

// Settings is the row config with defaults applied. Every field maps to
// an environment variable serve.py reads.
type Settings struct {
	Home     string // state dir: source checkout, venv, graph.db, config.yaml, serve.log
	Port     int    // MCP http port on 127.0.0.1
	LLM      string // openrouter | openai: which key in ~/.bough/env drives extraction
	Model    string // extraction model, in the provider's naming
	Embedder string // embedding model, same
}

// FromConfig applies defaults over a (possibly nil) row config.
func FromConfig(cfg map[string]any) Settings {
	home, _ := os.UserHomeDir()
	s := Settings{
		Home:     filepath.Join(home, ".bough", "graphiti"),
		Port:     8621,
		LLM:      "openrouter",
		Model:    "openai/gpt-5-mini",
		Embedder: "openai/text-embedding-3-small",
	}
	if v, ok := cfg["home"].(string); ok && v != "" {
		s.Home = v
	}
	switch v := cfg["port"].(type) {
	case int:
		s.Port = v
	case float64:
		s.Port = int(v)
	}
	if v, ok := cfg["llm"].(string); ok && v != "" {
		s.LLM = v
		if v == "openai" {
			s.Model, s.Embedder = "gpt-5-mini", "text-embedding-3-small"
		}
	}
	if v, ok := cfg["model"].(string); ok && v != "" {
		s.Model = v
	}
	if v, ok := cfg["embedder"].(string); ok && v != "" {
		s.Embedder = v
	}
	return s
}

func (s Settings) url() string    { return fmt.Sprintf("http://127.0.0.1:%d/mcp/", s.Port) }
func (s Settings) src() string    { return filepath.Join(s.Home, "src", "mcp_server") }
func (s Settings) python() string { return filepath.Join(s.src(), ".venv", "bin", "python") }
func (s Settings) log() string    { return filepath.Join(s.Home, "serve.log") }

func plistPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, "Library", "LaunchAgents", label+".plist")
}

func mcpJSONPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".bough", "mcp.json")
}

func hookPath(event string) string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".bough", "hooks", event, "graphiti.js")
}

// RenderHook fills a hook asset's BOUGH placeholder with the binary that
// should run `mcp call`: the hook runs in the codemode VM, whose shell
// has no promise of a `bough` on PATH.
func RenderHook(asset, bough string) (string, error) {
	b, err := assets.ReadFile("assets/" + asset)
	if err != nil {
		return "", err
	}
	return strings.ReplaceAll(string(b), "__BOUGH__", bough), nil
}

// RenderPlist fills the launchd template.
func RenderPlist(s Settings) (string, error) {
	b, err := assets.ReadFile("assets/launchd.plist")
	if err != nil {
		return "", err
	}
	r := strings.NewReplacer(
		"__PYTHON__", s.python(),
		"__HOME__", s.Home,
		"__PORT__", strconv.Itoa(s.Port),
		"__LLM__", s.LLM,
		"__MODEL__", s.Model,
		"__EMBEDDER__", s.Embedder,
	)
	return r.Replace(string(b)), nil
}

// MergeServer returns doc (a bough mcp.json, or empty) with servers.<name>
// set to a url entry; every other server and key survives. Pure.
func MergeServer(doc []byte, name, url string) ([]byte, error) {
	m := map[string]any{}
	if len(bytes.TrimSpace(doc)) > 0 {
		if err := json.Unmarshal(doc, &m); err != nil {
			return nil, fmt.Errorf("mcp.json: %w", err)
		}
	}
	servers, _ := m["servers"].(map[string]any)
	if servers == nil {
		servers = map[string]any{}
	}
	if url == "" {
		delete(servers, name)
	} else {
		servers[name] = map[string]any{"url": url}
	}
	m["servers"] = servers
	return json.MarshalIndent(m, "", "  ")
}

// EnsureRow returns yml with a `graphiti` row inserted after the `mcp`
// row, and whether it changed anything. A tree that already has the row,
// or has no `- id: mcp` row to anchor on, comes back untouched. Pure.
func EnsureRow(yml []byte) ([]byte, bool) {
	s := string(yml)
	if strings.Contains(s, "plugin: graphiti") {
		return yml, false
	}
	const anchor = "- id: mcp\n  plugin: mcp\n"
	i := strings.Index(s, anchor)
	if i < 0 {
		return yml, false
	}
	// The mcp row may carry an indented config/comment block; skip to the
	// next top-level line (or the end) before inserting.
	j := i + len(anchor)
	for j < len(s) {
		k := strings.IndexByte(s[j:], '\n')
		line := s[j:]
		if k >= 0 {
			line = s[j : j+k]
		}
		if line == "" || (!strings.HasPrefix(line, " ") && !strings.HasPrefix(line, "#")) {
			break
		}
		if k < 0 {
			j = len(s)
			break
		}
		j += k + 1
	}
	row := "\n# Long-term memory (bough graphiti install): prompt section only.\n- id: graphiti\n  plugin: graphiti\n"
	rest := s[j:]
	if rest != "" && !strings.HasPrefix(rest, "\n") {
		row += "\n"
	}
	return []byte(s[:j] + row + rest), true
}

// PromptSection is what the mounted row tells the model.
func PromptSection(s Settings) string {
	return "## memory\n" +
		"Long-term memory: a Graphiti knowledge graph behind the `graphiti` MCP server (" + s.url() + ").\n" +
		"Facts relevant to the prompt arrive as a [memory] block; the turn is remembered when it ends.\n" +
		"Look further with `bough mcp call graphiti/search_memory_facts '{\"query\":\"…\"}'`, entities with\n" +
		"`graphiti/search_nodes`, and save something explicitly with `graphiti/add_memory`."
}

type plugin struct{}

func init() {
	kernel.Register("graphiti", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "graphiti" }
func (plugin) Inject() []string { return nil }

// Apply only documents the memory to the model; the server is launchd's.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	s := FromConfig(cfg)
	if secs, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		secs.Set("memory", PromptSection(s))
		ctx.Effect(func() { secs.Set("memory", "") })
	}
	return nil
}

// Commands implements kernel.Commander.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "graphiti",
		Usage:   "install | start | stop | status | logs | uninstall",
		Summary: "self-hosted Graphiti memory: embedded FalkorDB + MCP server under launchd, hooks, mcp.json entry",
		Run:     runCLI,
	}}
}

func runCLI(cfg map[string]any, args []string) error {
	const usage = "usage: bough graphiti install | start | stop | status | logs | uninstall"
	if len(args) == 0 {
		return errors.New(usage)
	}
	s := FromConfig(cfg)
	switch args[0] {
	case "install":
		return install(s)
	case "start":
		return start(s)
	case "stop":
		return stop()
	case "status":
		return status(s)
	case "logs":
		return logs(s)
	case "uninstall":
		return uninstall(s)
	}
	return errors.New(usage)
}

func run(dir string, env []string, name string, args ...string) error {
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), env...)
	cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s %s: %w", name, strings.Join(args, " "), err)
	}
	return nil
}

func writeFile(path string, body []byte, mode os.FileMode) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, body, mode)
}

func install(s Settings) error {
	if _, err := exec.LookPath("uv"); err != nil {
		return errors.New("uv is required (brew install uv)")
	}
	if _, err := os.Stat("/opt/homebrew/opt/libomp/lib/libomp.dylib"); err != nil {
		fmt.Fprintln(os.Stderr, "warning: FalkorDB needs libomp at runtime: brew install libomp")
	}
	bough, err := os.Executable()
	if err != nil {
		return err
	}
	if resolved, err := filepath.EvalSymlinks(bough); err == nil {
		bough = resolved
	}

	// 1. Upstream mcp_server (sparse, shallow) and its venv, plus the embedded database.
	srcRoot := filepath.Join(s.Home, "src")
	if _, err := os.Stat(filepath.Join(srcRoot, ".git")); err != nil {
		if err := os.MkdirAll(s.Home, 0o755); err != nil {
			return err
		}
		fmt.Fprintln(os.Stderr, "cloning getzep/graphiti (mcp_server only)…")
		if err := run(s.Home, nil, "git", "clone", "-q", "--depth", "1", "--filter=blob:none", "--sparse", upstream, "src"); err != nil {
			return err
		}
		if err := run(srcRoot, nil, "git", "sparse-checkout", "set", "mcp_server"); err != nil {
			return err
		}
	} else {
		fmt.Fprintln(os.Stderr, "updating graphiti checkout…")
		if err := run(srcRoot, nil, "git", "pull", "-q", "--ff-only"); err != nil {
			fmt.Fprintf(os.Stderr, "warning: %v (keeping the current checkout)\n", err)
		}
	}
	fmt.Fprintln(os.Stderr, "syncing the server venv (python 3.12) and falkordblite…")
	env := []string{"UV_PYTHON=3.12"}
	if err := run(s.src(), env, "uv", "sync"); err != nil {
		return err
	}
	if err := run(s.src(), env, "uv", "pip", "install", "falkordblite"); err != nil {
		return err
	}

	// 2. Our files: launcher, config (kept if you edited it), hooks, mcp.json entry, plist.
	serve, _ := assets.ReadFile("assets/serve.py")
	if err := writeFile(filepath.Join(s.Home, "serve.py"), serve, 0o755); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(s.Home, "config.yaml")); err != nil {
		conf, _ := assets.ReadFile("assets/config.yaml")
		if err := writeFile(filepath.Join(s.Home, "config.yaml"), conf, 0o644); err != nil {
			return err
		}
	}
	for asset, event := range map[string]string{"stop.js": "stop", "prompt.js": "user-prompt-submit"} {
		body, err := RenderHook(asset, bough)
		if err != nil {
			return err
		}
		if err := writeFile(hookPath(event), []byte(body), 0o644); err != nil {
			return err
		}
	}
	doc, _ := os.ReadFile(mcpJSONPath())
	merged, err := MergeServer(doc, "graphiti", s.url())
	if err != nil {
		return err
	}
	if err := writeFile(mcpJSONPath(), merged, 0o644); err != nil {
		return err
	}
	plist, err := RenderPlist(s)
	if err != nil {
		return err
	}
	if err := writeFile(plistPath(), []byte(plist), 0o644); err != nil {
		return err
	}

	// The prompt-section row, in the global tree when there is one.
	home, _ := os.UserHomeDir()
	yml := filepath.Join(home, ".bough", "bough.yml")
	rowNote := "prompt section: add a `- id: graphiti / plugin: graphiti` row to your config tree"
	if b, err := os.ReadFile(yml); err == nil {
		if out, changed := EnsureRow(b); changed {
			if err := os.WriteFile(yml, out, 0o644); err != nil {
				return err
			}
			rowNote = "row added to " + yml
		} else if strings.Contains(string(b), "plugin: graphiti") {
			rowNote = "row present in " + yml
		}
	}

	// 3. Run it.
	if err := start(s); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "installed: %s\n  hooks: %s, %s\n  mcp.json: graphiti → %s\n  %s\n",
		s.Home, hookPath("stop"), hookPath("user-prompt-submit"), s.url(), rowNote)
	return nil
}

func domain() string { return "gui/" + strconv.Itoa(os.Getuid()) }

func start(s Settings) error {
	if _, err := os.Stat(plistPath()); err != nil {
		return errors.New("not installed: bough graphiti install")
	}
	// A fresh bootstrap picks up plist edits. bootout is asynchronous: wait
	// for the job to be gone, or bootstrap fails with "Input/output error".
	_ = exec.Command("launchctl", "bootout", domain()+"/"+label).Run()
	for i := 0; i < 100 && exec.Command("launchctl", "print", domain()+"/"+label).Run() == nil; i++ {
		time.Sleep(100 * time.Millisecond)
	}
	if err := run("", nil, "launchctl", "bootstrap", domain(), plistPath()); err != nil {
		return err
	}
	fmt.Fprint(os.Stderr, "starting")
	for i := 0; i < 90; i++ {
		if listening(s.Port) {
			fmt.Fprintf(os.Stderr, "\ngraphiti is up at %s\n", s.url())
			return nil
		}
		fmt.Fprint(os.Stderr, ".")
		time.Sleep(time.Second)
	}
	return fmt.Errorf("\nport %d never opened; see bough graphiti logs", s.Port)
}

func stop() error {
	if err := run("", nil, "launchctl", "bootout", domain()+"/"+label); err != nil {
		return err
	}
	fmt.Fprintln(os.Stderr, "stopped")
	return nil
}

func listening(port int) bool {
	c, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", port), 500*time.Millisecond)
	if err != nil {
		return false
	}
	c.Close()
	return true
}

func status(s Settings) error {
	fmt.Printf("home     %s\n", s.Home)
	fmt.Printf("server   %s  ", s.url())
	if listening(s.Port) {
		fmt.Println("LISTENING")
	} else {
		fmt.Println("down")
	}
	if _, err := os.Stat(plistPath()); err == nil {
		fmt.Printf("launchd  %s (loaded)\n", plistPath())
	} else {
		fmt.Println("launchd  not installed")
	}
	for _, ev := range []string{"user-prompt-submit", "stop"} {
		state := "missing"
		if _, err := os.Stat(hookPath(ev)); err == nil {
			state = "installed"
		}
		fmt.Printf("hook     %-20s %s\n", ev, state)
	}
	fmt.Printf("llm      %s / %s, embedder %s\n", s.LLM, s.Model, s.Embedder)
	return nil
}

func logs(s Settings) error {
	return run("", nil, "tail", "-n", "60", s.log())
}

// uninstall removes the job, hooks and mcp entry. The checkout and the
// graph under Home stay: data is not a thing a subcommand throws away.
func uninstall(s Settings) error {
	_ = exec.Command("launchctl", "bootout", domain()+"/"+label).Run()
	for _, p := range []string{plistPath(), hookPath("stop"), hookPath("user-prompt-submit")} {
		if err := os.Remove(p); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	if doc, err := os.ReadFile(mcpJSONPath()); err == nil {
		merged, err := MergeServer(doc, "graphiti", "")
		if err != nil {
			return err
		}
		if err := os.WriteFile(mcpJSONPath(), merged, 0o644); err != nil {
			return err
		}
	}
	fmt.Fprintf(os.Stderr, "uninstalled; %s (checkout + graph.db) kept\n", s.Home)
	return nil
}
