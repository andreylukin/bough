package ui

import (
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Live wiring shared by the tui and web modes across row remounts.
// The terminal (and the web listen address) can only be owned once
// per process, but hot reload remounts the ui row with a fresh inputs
// channel — so the running programs read events from a process-level
// broadcaster and send input through a swappable channel, and each
// mount just re-points both (same pattern as headless.go).

// historyView is the slice of the "history" service the UI reads.
// (*history.Store satisfies it; Append stays the loop's business.)
type historyView interface {
	Entries() []history.Entry
	Path() string
}

// commandsView is the slice of the "commands" service the UI uses
// (*commands.Registry satisfies it): the palette lists, dispatch runs.
type commandsView interface {
	List() []commands.CommandInfo
	Run(name, args string) (string, error)
}

// historyAppender is the Append slice of the "history" service, used
// to record "/" dispatches as "command"/"system" entries — NEVER
// "input", which DefaultProject would leak into model context.
type historyAppender interface {
	Append(kind string, data map[string]any) history.Entry
}

// askAnswers is the optional "ask-answers" service seam (the ask
// plugin's Asker satisfies it): resolving a pending tools.ask unblocks
// the tool call with text as its return value.
type askAnswers interface {
	Answer(id, text string) error
}

// contextLimiter is the optional seam the cost row's usage service
// satisfies: the current model's context window in tokens, 0 unknown.
type contextLimiter interface {
	ContextLimit() int
}

// contextFiles is the slice of the "context-md" service the startup
// header reads: the files in the system prompt.
type contextFiles interface {
	Loaded() []string
}

// skillNames is the slice of the "skills" service the startup header
// reads.
type skillNames interface {
	Names() []string
}

// uiCfg is one mount's immutable UI configuration; models read the
// current one through liveCfg on every render, so a remount (new
// theme/keymap row, history landing) restyles running views.
type uiCfg struct {
	theme   theme
	keys    map[string]string // action -> key (plus "leader" and "chord:<key>" -> action)
	action  map[string]string // key -> action (derived)
	chords  map[string]string // key after the leader -> action (derived)
	status  string            // status-bar left text
	hist    historyView       // nil when no history service
	usage   llm.UsageReporter // the "usage" (cost row) or llm service; nil when neither reports
	modeler llm.Modeler       // the llm service when it names its model; nil otherwise
	effort  llm.Efforter      // the llm service when its thinking level can be changed; nil otherwise
	// small is the "llm-small" service and ONLY that: the status-line
	// label and the composer's guess are worth a cheap model's time,
	// never the agent's own (in money or in latency).
	small llm.LLM
	// jobs is the background-job service, for the strip under the
	// composer; nil when the tools row is absent.
	jobs jobLister
	// prs is the pr-watch service, for the status bar's count.
	prs bgCounter
	// board is the attention service, for the board at the top.
	board boardSource
	// past is this directory's prompts from earlier sessions, newest
	// first, for the composer's Up arrow. A func because reading them
	// is file work: the composer calls it once, on the first recall.
	// nil when the launcher provides no history directory.
	past     func() []string
	limit    contextLimiter // the usage service when it knows the model's context window; nil otherwise
	mdStyle  string         // "dark"/"light" glamour override; "" = detect
	notice   string         // launcher "notice" service: a first-row warning (stale binary)
	collapse string         // "all" | "large" | "none": which code/result blocks start collapsed
	draft    string         // text the composer opens with (a link that starts a chat about something)

	// "/" command seam: with no commands service, "/" is plain text
	// and the palette never opens. hlog records dispatches to history
	// ("command"/"system" entries); nil is fine (no recording).
	cmds commandsView
	hlog historyAppender

	// Startup-header seams (welcomeView): both optional.
	ctxmd  contextFiles
	skills skillNames

	// "cancel" seam (the loop's turn cancel); nil = nothing to stop.
	cancel func()
	// "steer" seam (the loop's mid-turn message); nil = enter queues
	// a follow-up while a turn runs, as it always did.
	steer func(string) bool

	// "ask" seam: nil when no ask plugin is mounted (ask events then
	// render pending forever and the composer never routes answers).
	ask askAnswers

	// Session-resume seam (all optional, provided by the launcher):
	// "session-picker" marker present => a new model starts in the
	// picker; "sessions" is the picker's rows (newest first);
	// "session-choose" is called exactly once with the chosen session
	// id ("" = fresh session) and must have swapped the "history"
	// service to the chosen session by the time it returns.
	picker   bool
	sessions []history.SessionInfo
	choose   func(string) // nil with sessions present => read-only list
}

var (
	liveMu     sync.Mutex
	liveInputs chan<- string // current mount's inputs; nil while unmounted
	liveB      = &broadcaster{subs: map[int]chan Event{}}
	liveCfg    atomic.Pointer[uiCfg]
)

func init() {
	liveCfg.Store(newCfg(defaultTheme(), defaultKeymap(), "bough", nil))
}

func newCfg(t theme, keys map[string]string, status string, hist historyView) *uiCfg {
	action := make(map[string]string, len(keys))
	chords := map[string]string{}
	for a, k := range keys {
		if c, ok := strings.CutPrefix(a, chordPrefix); ok {
			chords[c] = k // k is the action here
			continue
		}
		if k != "" { // unbound (config-only) actions
			action[k] = a
		}
	}
	return &uiCfg{theme: t, keys: keys, action: action, chords: chords, status: status, hist: hist, collapse: "all"}
}

// buildCfg reads the optional "theme", "keymap" and "history" services
// (Get during Apply registers them as live dependencies: providing one
// later remounts the ui row) plus the row table for the status bar.
// rowCfg is the ui row's config: collapse: "all" (default) | "large" |
// "none" picks what starts closed. Under "all" every detail block —
// code, result, thinking, a subagent card, and a MULTI-LINE note
// (error, system, todo) — opens as a one-row header; a one-line note
// stays as it is, since its header would be longer than the line.
func buildCfg(ctx *kernel.Context, rowCfg map[string]any) (*uiCfg, error) {
	t := defaultTheme()
	mdStyle := ""
	// The "palette" (theme row: a whole bundled scheme) goes under the
	// "theme" (init.js: per-token overrides).
	for _, key := range []string{"palette", "theme"} {
		m, ok, err := getStringMap(ctx, key)
		if err != nil {
			return nil, err
		}
		if !ok {
			continue
		}
		// "markdown" is not a style token: it overrides the glamour
		// style ("dark"/"light") picked from the detected background.
		if v, has := m["markdown"]; has {
			if v != "dark" && v != "light" {
				return nil, fmt.Errorf("ui: theme: markdown must be \"dark\" or \"light\", got %q", v)
			}
			mdStyle = v
			m2 := make(map[string]string, len(m))
			for k, vv := range m {
				if k != "markdown" {
					m2[k] = vv
				}
			}
			m = m2
		}
		if err := t.apply(m); err != nil {
			return nil, err
		}
	}
	keys := defaultKeymap()
	if m, ok, err := getStringMap(ctx, "keymap"); err != nil {
		return nil, err
	} else if ok {
		if err := applyKeymap(keys, m); err != nil {
			return nil, err
		}
	}
	var hist historyView
	if h, err := kernel.Get[historyView](ctx, "history"); err == nil {
		hist = h
	}
	var cmds commandsView
	if c, err := kernel.Get[commandsView](ctx, "commands"); err == nil {
		cmds = c
	}
	var hlog historyAppender
	if a, err := kernel.Get[historyAppender](ctx, "history"); err == nil {
		hlog = a
	}

	// Status bar identity: "bough · <model>" (the provider name when
	// the row names no model); the bar keeps only "bough" once the llm
	// names its model itself (statusBar).
	status := "bough"
	for _, r := range ctx.Desired() {
		if r.ID == "llm" || strings.HasPrefix(r.Plugin, "llm-") {
			status = "bough · " + r.Plugin
			if mdl, ok := r.Config["model"].(string); ok && mdl != "" {
				status = "bough · " + mdl
			}
		}
	}
	cfg := newCfg(t, keys, status, hist)
	cfg.mdStyle = mdStyle
	if n, err := kernel.Get[string](ctx, "notice"); err == nil {
		cfg.notice = n
	}
	cfg.cmds = cmds
	cfg.hlog = hlog
	if c, err := kernel.Get[contextFiles](ctx, "context-md"); err == nil {
		cfg.ctxmd = c
	}
	if s, err := kernel.Get[skillNames](ctx, "skills"); err == nil {
		cfg.skills = s
	}
	// The cost row's priced view first; the llm's own tally otherwise.
	if u, err := kernel.Get[llm.UsageReporter](ctx, "usage"); err == nil {
		cfg.usage = u
	} else if u, err := kernel.Get[llm.UsageReporter](ctx, "llm"); err == nil {
		cfg.usage = u
	}
	if l, ok := cfg.usage.(contextLimiter); ok {
		cfg.limit = l
	}
	if m, err := kernel.Get[llm.Modeler](ctx, "llm"); err == nil {
		cfg.modeler = m
	}
	if e, err := kernel.Get[llm.Efforter](ctx, "llm"); err == nil {
		cfg.effort = e
	}
	if s, err := kernel.Get[llm.LLM](ctx, llm.SmallKey); err == nil {
		cfg.small = s
	} else if smallRowConfigured(ctx) {
		// The row is there but the service is not: either the row is
		// missing `service: llm-small` (so it published under "llm"
		// and quietly REPLACED the agent's model), or the binary
		// predates that config key. Both are silent otherwise — the
		// kernel's warning goes to stderr, which under `bough web` is
		// a log file nobody reads.
		cfg.notice = strings.TrimSpace(cfg.notice + "\nan llm-small row is configured but provides no llm-small service: add `service: llm-small` to its config, or run `bough update` if this binary predates it")
	}
	if j, err := kernel.Get[jobLister](ctx, "job-notices"); err == nil {
		cfg.jobs = j
	}
	if p, err := kernel.Get[bgCounter](ctx, "pr-watch"); err == nil {
		cfg.prs = p
	}
	if b, err := kernel.Get[boardSource](ctx, "attention"); err == nil {
		cfg.board = b
	}
	// A provider that cannot run says so now, not after the user has
	// typed a prompt and waited. Without this the TUI opens on a clean
	// welcome and gives no sign that there is no API key — the single
	// most likely state for someone running bough for the first time.
	if r, err := kernel.Get[llm.Ready](ctx, "llm"); err == nil {
		if e := r.Ready(); e != nil {
			cfg.notice = strings.TrimSpace(cfg.notice + "\n" + e.Error())
		}
	}
	if a, err := kernel.Get[askAnswers](ctx, "ask-answers"); err == nil {
		cfg.ask = a
	}
	if c, err := kernel.Get[func()](ctx, "cancel"); err == nil {
		cfg.cancel = c
	}
	if s, err := kernel.Get[func(string) bool](ctx, "steer"); err == nil {
		cfg.steer = s
	}
	if _, err := kernel.Get[any](ctx, "session-picker"); err == nil {
		cfg.picker = true
	}
	if s, err := kernel.Get[[]history.SessionInfo](ctx, "sessions"); err == nil {
		cfg.sessions = s
	}
	if p, err := kernel.Get[func() []string](ctx, "prompt-history"); err == nil {
		cfg.past = p
	}
	if c, err := kernel.Get[func(string)](ctx, "session-choose"); err == nil {
		cfg.choose = c
	}
	if v, has := rowCfg["collapse"]; has {
		s, ok := v.(string)
		if !ok || (s != "all" && s != "large" && s != "none") {
			return nil, fmt.Errorf("ui: collapse must be \"all\", \"large\" or \"none\", got %v", v)
		}
		cfg.collapse = s
	}
	if v, has := rowCfg["draft"]; has {
		cfg.draft, _ = v.(string)
	}
	return cfg, nil
}

// getStringMap fetches a map[string]string service, tolerating the
// map[string]any shape a JS-provided config arrives as. Absent is
// fine (ok=false); present with a non-string shape fails loud.
func getStringMap(ctx *kernel.Context, key string) (map[string]string, bool, error) {
	v, err := kernel.Get[any](ctx, key)
	if err != nil {
		return nil, false, nil // absent
	}
	switch m := v.(type) {
	case map[string]string:
		return m, true, nil
	case map[string]any:
		out := make(map[string]string, len(m))
		for k, av := range m {
			s, ok := av.(string)
			if !ok {
				return nil, false, fmt.Errorf("ui: service %q: value of %q is %T, not string", key, k, av)
			}
			out[k] = s
		}
		return out, true, nil
	default:
		return nil, false, fmt.Errorf("ui: service %q is %T, not map[string]string", key, v)
	}
}

// attachLive points the live wiring at this mount: swap the config
// and inputs, bridge kernel loop/events into the process broadcaster
// (the On subscription is auto-disposed with the row), and detach
// inputs on unmount so nothing sends into a closed channel.
func attachLive(ctx *kernel.Context, inputs chan<- string, cfg *uiCfg) {
	liveCfg.Store(cfg)
	liveMu.Lock()
	liveInputs = inputs
	liveMu.Unlock()
	ctx.On("loop/event", func(payload any) {
		liveB.publish(eventOf(payload))
	})
	ctx.Effect(func() {
		liveMu.Lock()
		if liveInputs == inputs {
			liveInputs = nil
		}
		liveMu.Unlock()
	})
}

// smallRowConfigured reports whether the config tree means to provide
// a small model: a row named llm-small, or any row asking for that
// service key.
func smallRowConfigured(ctx *kernel.Context) bool {
	for _, r := range ctx.Desired() {
		if r.ID == llm.SmallKey {
			return true
		}
		if s, ok := r.Config["service"].(string); ok && s == llm.SmallKey {
			return true
		}
	}
	return false
}

// sendLive delivers one input line to the current mount, waiting out
// a remount window (sending under the lock, like headless: the row
// disposer blocks until an in-flight send finishes). After ~5s with
// no loop attached the line is dropped loudly.
func sendLive(line string) {
	for range 100 {
		liveMu.Lock()
		ch := liveInputs
		if ch != nil {
			ch <- line
			liveMu.Unlock()
			return
		}
		liveMu.Unlock()
		time.Sleep(50 * time.Millisecond)
	}
	fmt.Fprintln(os.Stderr, "ui: input dropped: no loop attached after 5s")
}
