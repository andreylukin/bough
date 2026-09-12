// Package hooks is the "hooks-js" plugin: hook files are .js bodies
// run in the shared codemode VM. It provides the "hooks" service
// (loop.Hooks). Files live in ~/.bough/hooks/<event>/*.js and
// ./.bough/hooks/<event>/*.js; a project file shadows a global one
// with the same base name. Files are re-read on every fire.
package hooks

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/offlist"
)

// runHooker is the slice of the codemode service we need.
type runHooker interface {
	RunHook(ctx context.Context, fileBody string, event map[string]any) (map[string]any, error)
}

// Service implements the "hooks" service (loop.Hooks).
type Service struct {
	code runHooker

	mu      sync.Mutex
	gohs    map[string][]goHook // in-process hooks by event
	fires   []Fire              // ring of the last fireRing records, oldest first
	pending []Fire              // recorded but not yet drained to history
	session string              // the session the fires belong to
}

// fireRing bounds the ledger: hooks fire on every tool call
// (pre-code-exec, post-result), so an unbounded slice would grow with
// the session.
const fireRing = 200

// maxHookOutput bounds what one hook may push into the model's context
// through a structured key. Unbounded, a hook rewriting a result could
// spend the whole window without anyone having asked for it; the same
// 10,000 characters Claude Code allows.
const maxHookOutput = 10000

// Fire is one hook run: what ran, how long it took, and what it
// decided. The json tags are the /api/hooks "fires" contract.
type Fire struct {
	At       time.Time `json:"at"`
	Session  string    `json:"session"`
	Event    string    `json:"event"`
	Name     string    `json:"name"`
	Ms       int64     `json:"ms"`
	Decision string    `json:"decision"`
	Error    string    `json:"error"`
	// Notice is the hook's message to the human. It is recorded and
	// shown, and never reaches the model.
	Notice string `json:"notice"`
	// Truncated names the keys capped at maxHookOutput, so a shortened
	// result is visible rather than mysterious.
	Truncated []string `json:"truncated"`
}

// SetSession names the session the fires that follow belong to. The
// loop sets it around a fire: the Hooks seam is one call wide, and
// threading a session through it would widen the hot path.
func (s *Service) SetSession(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.session = id
}

// record appends one fire to the ring and to the undrained queue.
func (s *Service) record(f Fire) {
	s.mu.Lock()
	defer s.mu.Unlock()
	f.Session = s.session
	s.fires = append(s.fires, f)
	s.pending = append(s.pending, f)
	if len(s.fires) > fireRing {
		s.fires = append(s.fires[:0:0], s.fires[len(s.fires)-fireRing:]...)
	}
}

// TakeFires drains the fires recorded since the last drain. The ring
// above only outlives the call; `bough serve` is a different process
// and can read nothing from this one's memory, so the loop drains
// after every fire and writes them to the session history, which both
// processes share. Without this the ledger is invisible to the web.
// TakeFireRecords drains the fires recorded since the last drain, as
// history data. Maps rather than Fire values because plugins/loop
// consumes this and must not import this package: the in-package test
// here imports the loop, so the pair would be a cycle.
func (s *Service) TakeFireRecords() []map[string]any {
	out := make([]map[string]any, 0, len(s.pending))
	for _, f := range s.TakeFires() {
		rec := map[string]any{
			"event": f.Event, "name": f.Name, "ms": f.Ms,
			"decision": f.Decision, "error": f.Error,
		}
		// Absent rather than empty: the loop decides whether a fire is
		// worth keeping by looking for these, and a present-but-empty
		// value would make every fire look interesting.
		if f.Notice != "" {
			rec["notice"] = f.Notice
		}
		if len(f.Truncated) > 0 {
			rec["truncated"] = f.Truncated
		}
		out = append(out, rec)
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

func (s *Service) TakeFires() []Fire {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.pending) == 0 {
		return nil
	}
	out := s.pending
	s.pending = nil
	return out
}

// Fires returns the most recent fires, newest first. limit <= 0 means
// everything the ring holds.
func (s *Service) Fires(limit int) []Fire {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := len(s.fires)
	if limit > 0 && limit < n {
		n = limit
	}
	out := make([]Fire, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, s.fires[len(s.fires)-1-i])
	}
	return out
}

// decision reads what a hook result did to the payload: a block or a
// deny short-circuits the event, and any key of the payload the
// result replaced is a rewrite. Anything else is a pass.
func decision(payload, res map[string]any) string {
	if res == nil {
		return ""
	}
	if _, ok := res["block"]; ok {
		return "blocked"
	}
	if _, ok := res["deny"]; ok {
		return "denied"
	}
	for k, v := range res {
		if k == "notice" {
			continue // the diagnostic channel decides nothing
		}
		if old, ok := payload[k]; ok && !reflect.DeepEqual(old, v) {
			return "rewrote"
		}
	}
	return ""
}

// notice reads a hook's message to the human out of its result.
func notice(res map[string]any) string {
	n, _ := res["notice"].(string)
	return n
}

// mergeInto merges one hook result into the merged result. Later keys
// overwrite, except notices: two hooks with something to say both get
// to say it.
func mergeInto(merged, res map[string]any) {
	if n := notice(res); n != "" {
		if had := notice(merged); had != "" {
			n = had + "\n" + n
		}
		res = maps.Clone(res)
		res["notice"] = n
	}
	maps.Copy(merged, res)
}

// capKeys truncates the keys a hook can push into the model's context and
// returns the names of the ones it cut. Only these three are read at a
// fire site, so only these three can reach the model.
func capKeys(res map[string]any) []string {
	var cut []string
	for _, k := range []string{"code", "input", "result"} {
		s, ok := res[k].(string)
		if !ok || len(s) <= maxHookOutput {
			continue
		}
		n := maxHookOutput
		for n > 0 && !utf8.RuneStart(s[n]) {
			n-- // never cut a rune in half
		}
		res[k] = s[:n] + fmt.Sprintf("\n[hook output truncated at %d characters]", maxHookOutput)
		cut = append(cut, k)
	}
	return cut
}

// off reports whether the user has turned this hook off. A hook's id is
// "<event>/<name>", the same id the API reports for the row.
func off(event, name string) bool {
	home, err := os.UserHomeDir()
	if err != nil {
		return false
	}
	return offlist.Load(filepath.Join(home, ".bough")).Off("hook", event+"/"+name)
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
// that fails to read or run is logged to stderr and skipped. A hook
// the user has turned off in off.yml does not run at all.
// No hook files, or none returning anything, is a nil result.
func (s *Service) Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error) {
	var merged map[string]any
	s.mu.Lock()
	gohs := append([]goHook(nil), s.gohs[event]...)
	s.mu.Unlock()
	for _, h := range gohs {
		if off(event, h.name) {
			continue
		}
		start := time.Now()
		res := h.fn(payload)
		cut := capKeys(res)
		s.record(Fire{At: start, Event: event, Name: h.name,
			Ms: time.Since(start).Milliseconds(), Decision: decision(payload, res),
			Notice: notice(res), Truncated: cut})
		if res == nil {
			continue
		}
		if merged == nil {
			merged = map[string]any{}
		}
		mergeInto(merged, res)
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
	// A hook that throws used to be written straight to os.Stderr. Under
	// the TUI that lands inside the alt-screen and scribbles over the
	// frame — the error text spliced itself into the composer's draft
	// line and pushed the composer up the screen. Failures are returned
	// instead, so the loop renders them as error events and history
	// records them.
	var failed []error
	for _, path := range hookFiles(event) {
		name := filepath.Base(path)
		if off(event, name) {
			continue
		}
		start := time.Now()
		body, err := os.ReadFile(path)
		if err != nil {
			s.record(Fire{At: start, Event: event, Name: name,
				Ms: time.Since(start).Milliseconds(), Error: err.Error()})
			failed = append(failed, fmt.Errorf("%s: %w", path, err))
			continue
		}
		res, err := s.code.RunHook(ctx, string(body), payload)
		if ctx.Err() != nil {
			return merged, nil // the turn was cancelled: not a hook failure
		}
		if err != nil {
			s.record(Fire{At: start, Event: event, Name: name,
				Ms: time.Since(start).Milliseconds(), Error: err.Error()})
			failed = append(failed, fmt.Errorf("%s: %w", path, err))
			continue
		}
		cut := capKeys(res)
		s.record(Fire{At: start, Event: event, Name: name,
			Ms: time.Since(start).Milliseconds(), Decision: decision(payload, res),
			Notice: notice(res), Truncated: cut})
		if res == nil {
			continue
		}
		if merged == nil {
			merged = map[string]any{}
		}
		mergeInto(merged, res)
		if _, ok := res["block"]; ok {
			break
		}
		if _, ok := res["deny"]; ok {
			break
		}
	}
	// errors.Join is nil when nothing failed, so a clean run is
	// unchanged; merged still comes back, because one bad hook file must
	// not void what the others contributed.
	return merged, errors.Join(failed...)
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
