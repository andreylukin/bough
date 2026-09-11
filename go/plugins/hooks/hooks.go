// Package hooks is the "hooks-js" plugin: hook files are .js bodies
// run in the shared codemode VM. It provides the "hooks" service
// (loop.Hooks). Files live in ~/.bough/hooks/<event>/*.js and
// ./.bough/hooks/<event>/*.js; a project file shadows a global one
// with the same base name. Files are re-read on every fire.
package hooks

import (
	"context"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
)

// runHooker is the slice of the codemode service we need.
type runHooker interface {
	RunHook(ctx context.Context, fileBody string, event map[string]any) (map[string]any, error)
}

// Service implements the "hooks" service (loop.Hooks).
type Service struct {
	code runHooker

	mu   sync.Mutex
	gohs map[string][]goHook // in-process hooks by event
}

// goHook is a hook written in Go by another row (the rules row's
// scoped rules, say). It runs before the .js files for its event and
// merges the same way. A nil result is "nothing to say".
type goHook struct {
	name string
	fn   func(payload map[string]any) map[string]any
}

// Add registers an in-process hook for event; the returned function
// removes it (a row calls it from its Effect).
func (s *Service) Add(event, name string, fn func(payload map[string]any) map[string]any) func() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.gohs == nil {
		s.gohs = map[string][]goHook{}
	}
	s.gohs[event] = append(s.gohs[event], goHook{name, fn})
	return func() {
		s.mu.Lock()
		defer s.mu.Unlock()
		hs := s.gohs[event]
		for i, h := range hs {
			if h.name == name {
				s.gohs[event] = append(hs[:i:i], hs[i+1:]...)
				return
			}
		}
	}
}

// Fire runs every hook file for event, in base-name order, project
// shadowing global. Results merge in order (later keys overwrite);
// a "block" or "deny" key short-circuits remaining files. A file
// that fails to read or run is logged to stderr and skipped.
// No hook files, or none returning anything, is a nil result.
func (s *Service) Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error) {
	var merged map[string]any
	s.mu.Lock()
	gohs := append([]goHook(nil), s.gohs[event]...)
	s.mu.Unlock()
	for _, h := range gohs {
		res := h.fn(payload)
		if res == nil {
			continue
		}
		if merged == nil {
			merged = map[string]any{}
		}
		maps.Copy(merged, res)
		if _, ok := res["block"]; ok {
			return merged, nil
		}
		if _, ok := res["deny"]; ok {
			return merged, nil
		}
		// A later hook sees what an earlier one rewrote.
		for k, v := range res {
			if _, ok := payload[k]; ok {
				payload[k] = v
			}
		}
	}
	for _, path := range hookFiles(event) {
		body, err := os.ReadFile(path)
		if err != nil {
			fmt.Fprintf(os.Stderr, "hooks: %s: %v\n", path, err)
			continue
		}
		res, err := s.code.RunHook(ctx, string(body), payload)
		if ctx.Err() != nil {
			return merged, nil // the turn was cancelled: not a hook failure
		}
		if err != nil {
			fmt.Fprintf(os.Stderr, "hooks: %s: %v\n", path, err)
			continue
		}
		if res == nil {
			continue
		}
		if merged == nil {
			merged = map[string]any{}
		}
		maps.Copy(merged, res)
		if _, ok := res["block"]; ok {
			break
		}
		if _, ok := res["deny"]; ok {
			break
		}
	}
	return merged, nil
}

// hookFiles lists the .js files for event: ~/.bough/hooks/<event>/
// then ./.bough/hooks/<event>/, project shadowing global on the same
// base name, sorted by base name. Missing dirs are fine.
func hookFiles(event string) []string {
	byName := map[string]string{}
	var dirs []string
	if home, err := os.UserHomeDir(); err == nil {
		dirs = append(dirs, filepath.Join(home, ".bough", "hooks", event))
	}
	dirs = append(dirs, filepath.Join(".bough", "hooks", event))
	for _, dir := range dirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue // missing dir = no hooks
		}
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".js") {
				continue
			}
			byName[e.Name()] = filepath.Join(dir, e.Name())
		}
	}
	names := slices.Sorted(maps.Keys(byName))
	paths := make([]string, len(names))
	for i, n := range names {
		paths[i] = byName[n]
	}
	return paths
}

type plugin struct{}

func init() {
	kernel.Register("hooks-js", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "hooks-js" }
func (plugin) Inject() []string { return []string{"codemode"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	code, err := kernel.Get[runHooker](ctx, "codemode")
	if err != nil {
		return err
	}
	s := &Service{code: code}
	ctx.Provide("hooks", s)
	ctx.Effect(func() {
		_, _ = s.Fire(context.Background(), "session-end", map[string]any{})
	})
	return nil
}
