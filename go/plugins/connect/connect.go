// Package connect is the "connect" plugin: /connect, which adds a
// provider API key and switches to it without leaving the session.
//
// bough tells a newcomer at launch that their provider has no key and
// names the file to put one in — and then they have to quit, edit
// ~/.bough/env, and start again. opencode's /connect closes that loop;
// this is the same idea. It also answers "which providers am I set up
// for?", which otherwise means reading an env file.
//
// The key is written to ~/.bough/env (read at every boot) and set in
// this process, then the llm row is swapped to that provider through
// the "config-set" seam — the same path /model uses. The command is
// marked Secret, so neither the transcript nor the history file ever
// holds the key.
package connect

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

func init() {
	kernel.Register("connect", func() kernel.Plugin { return plugin{} })
}

// provider is one thing /connect can set up: the env var that carries
// its key, the plugin that reads it, and a model to start on.
type provider struct {
	env    string
	plugin string
	model  string
}

// providers is the set bough ships plugins for.
var providers = map[string]provider{
	"anthropic":  {"ANTHROPIC_API_KEY", "llm-anthropic", "claude-sonnet-5"},
	"openrouter": {"OPENROUTER_API_KEY", "llm-openrouter", "anthropic/claude-sonnet-5"},
	"openai":     {"OPENAI_API_KEY", "llm-openai", "gpt-5.6"},
	"cerebras":   {"CEREBRAS_API_KEY", "llm-cerebras", "gpt-oss-120b"},
}

type plugin struct{}

func (plugin) Name() string     { return "connect" }
func (plugin) Inject() []string { return []string{"commands"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}
	// Optional: without it /connect can still record a key, it just
	// cannot switch the running session to it.
	set, _ := kernel.Get[func(...string) error](ctx, "config-set")

	path, _ := cfg["env_file"].(string)
	if path == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("connect: %w", err)
		}
		path = filepath.Join(home, ".bough", "env")
	}

	info := commands.CommandInfo{
		Name:    "connect",
		Usage:   "[provider [key]]",
		Summary: "add a provider API key and switch to it",
		Secret:  true,
	}
	if err := reg.Register(info, func(args string) (string, error) {
		return run(path, set, args)
	}); err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	ctx.Effect(func() { reg.Unregister("connect") })
	return nil
}

// run is /connect. No arguments lists what is set up; a provider alone
// reports on it; a provider and a key records the key and switches.
func run(envPath string, set func(...string) error, args string) (string, error) {
	name, key, _ := strings.Cut(strings.TrimSpace(args), " ")
	name, key = strings.ToLower(strings.TrimSpace(name)), unquote(strings.TrimSpace(key))

	if name == "" {
		return list(), nil
	}
	p, ok := providers[name]
	if !ok {
		return "", fmt.Errorf("unknown provider %q; try one of: %s", name, strings.Join(names(), ", "))
	}
	if key == "" {
		if os.Getenv(p.env) == "" {
			return fmt.Sprintf("%s is not set.\nRun: /connect %s <key>\nThe key is written to %s and never echoed.",
				p.env, name, tildify(envPath)), nil
		}
		if set == nil {
			return p.env + " is set. This session cannot switch providers (no config-set).", nil
		}
		if err := switchTo(set, p); err != nil {
			return "", err
		}
		return fmt.Sprintf("switched to %s (%s); %s was already set", p.plugin, p.model, p.env), nil
	}

	if err := writeKey(envPath, p.env, key); err != nil {
		return "", err
	}
	// This process reads keys from the environment, so the running
	// session picks it up without a restart.
	if err := os.Setenv(p.env, key); err != nil {
		return "", fmt.Errorf("connect: %w", err)
	}
	out := fmt.Sprintf("wrote %s to %s", p.env, tildify(envPath))
	if set == nil {
		return out + "\nRestart bough to use it.", nil
	}
	if err := switchTo(set, p); err != nil {
		return out + "\n" + err.Error(), nil
	}
	return out + fmt.Sprintf("\nswitched to %s (%s) — /model changes it", p.plugin, p.model), nil
}

// switchTo points the llm row at a provider, the same way /model does.
func switchTo(set func(...string) error, p provider) error {
	if err := set("llm.plugin="+p.plugin, "llm.model="+p.model); err != nil {
		return fmt.Errorf("could not switch to %s: %w", p.plugin, err)
	}
	return nil
}

// list reports which providers have a key, without printing any.
func list() string {
	var b strings.Builder
	b.WriteString("providers (a key in the environment or ~/.bough/env):\n")
	for _, n := range names() {
		p := providers[n]
		state := "—"
		if os.Getenv(p.env) != "" {
			state = "set"
		}
		fmt.Fprintf(&b, "  %-11s %-20s %s\n", n, p.env, state)
	}
	b.WriteString("\n/connect <provider> <key> records a key and switches to it.")
	return b.String()
}

func names() []string {
	out := make([]string, 0, len(providers))
	for n := range providers {
		out = append(out, n)
	}
	sort.Strings(out)
	return out
}

// writeKey appends KEY=value to the env file, replacing any line that
// already sets that variable. The file is 0600: it holds credentials.
func writeKey(path, env, key string) error {
	if strings.ContainsAny(key, "\n\r") {
		return fmt.Errorf("connect: a key cannot contain a newline")
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	var kept []string
	if b, err := os.ReadFile(path); err == nil {
		for _, line := range strings.Split(string(b), "\n") {
			if k, _, ok := strings.Cut(line, "="); !ok || strings.TrimSpace(k) != env {
				kept = append(kept, line)
			}
		}
	}
	for len(kept) > 0 && strings.TrimSpace(kept[len(kept)-1]) == "" {
		kept = kept[:len(kept)-1]
	}
	kept = append(kept, env+"="+key, "")
	// Written whole and renamed: a half-written env file loses keys the
	// user already had.
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(strings.Join(kept, "\n")), 0o600); err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	return nil
}

// tildify shortens a path under $HOME for display.
func tildify(p string) string {
	if home, err := os.UserHomeDir(); err == nil {
		if rest, ok := strings.CutPrefix(p, home+string(filepath.Separator)); ok {
			return "~/" + rest
		}
	}
	return p
}

// unquote strips one layer of surrounding quotes, which is how a key
// arrives when it is pasted from a shell `export KEY="..."` line. The
// env-file loader does the same on the way in (cmd/bough/envfile.go);
// storing the quotes instead sends the provider `Bearer "sk-…"` and
// earns a 401 that blames the key.
func unquote(v string) string {
	if len(v) >= 2 && (v[0] == '"' || v[0] == '\'') && v[len(v)-1] == v[0] {
		return v[1 : len(v)-1]
	}
	return v
}
