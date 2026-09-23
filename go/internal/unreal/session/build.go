//go:build !windows

package session

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/coordinator"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/tool"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal"
	"github.com/andreylukin/bough/internal/unreal/boughcall"
	"github.com/andreylukin/bough/internal/unreal/prompt"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// storeFormat is localfile's format at the pin; a bump means a
// migration or an explicit lossy reseed (§16), never a silent one.
const storeFormat = 2

// seedCap is how much of an earlier transcript a seed carries: the tail,
// because the end of a session is what the next message is about.
const seedCap = 400 << 10

// jsTool is the run_js tool's name when tools: both, else "".
func (r *Runtime) jsTool() string {
	if r.cfg.Tools == "both" {
		return "run_js"
	}
	return ""
}

// snapshot is the tool set a coordinator is built with.
func (r *Runtime) snapshot() []agenttools.Tool {
	if r.d.Tools == nil {
		return nil
	}
	var out []agenttools.Tool
	for _, t := range r.d.Tools.Tools() {
		if t.Name == "run_js" && r.cfg.Tools != "both" {
			continue
		}
		out = append(out, t)
	}
	return out
}

func (r *Runtime) toolsHash() string { return toolreg.Hash(r.snapshot()) }

// detail is the call-row text for a call, from the live registry: the
// projector outlives any one snapshot.
func (r *Runtime) detail(name string, args json.RawMessage) string {
	return toolreg.Detail(r.snapshot())(name, args)
}

func renderCall(callID string, status tool.CallStatus, ops []operation.Operation) (string, toolreg.Handle, bool) {
	return toolreg.Render(callID, status, ops)
}

// newHandler is the bough.call handler for the main session or a child.
func (r *Runtime) newHandler(worker string, progress func(id, text string)) *boughcall.Handler {
	lookup := func(string) (agenttools.Tool, bool) { return agenttools.Tool{}, false }
	if r.d.Tools != nil {
		lookup = r.d.Tools.Lookup
	}
	return boughcall.New(boughcall.Options{
		Tools:       lookup,
		Hooks:       lazyHooks{r},
		Redact:      r.d.Redact,
		CallTimeout: r.cfg.CallTimeout,
		SpillDir: func() string {
			if r.d.Scratch != nil {
				if s := r.d.Scratch(); s != "" {
					return filepath.Join(s, "calls")
				}
			}
			return filepath.Join(r.d.Store, "calls")
		},
		Session:  r.d.SessionID,
		Worker:   worker,
		Progress: progress,
	})
}

// lazyHooks reads the hooks seam per call: the hooks row may mount,
// swap or go away while the session runs.
type lazyHooks struct{ r *Runtime }

func (h lazyHooks) get() agenttools.Hooks {
	if h.r.d.Hooks == nil {
		return nil
	}
	return h.r.d.Hooks()
}

func (h lazyHooks) PreTool(ctx context.Context, tool string, c agenttools.Call, detail string) (json.RawMessage, string) {
	if x := h.get(); x != nil {
		return x.PreTool(ctx, tool, c, detail)
	}
	return nil, ""
}

func (h lazyHooks) PostTool(ctx context.Context, tool string, c agenttools.Call, detail string, res agenttools.Result) agenttools.Result {
	if x := h.get(); x != nil {
		return x.PostTool(ctx, tool, c, detail, res)
	}
	return res
}

// open reads the session's history to decide what the store is: this
// session's own (resume, then catch up), a parent's (a fork, built at
// the first input), or none yet (fresh, or a loop session to seed).
func (r *Runtime) open(ctx context.Context) error {
	a := &r.a
	entries := r.d.History.Entries()
	var eng *history.Entry
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].Kind == "engine" {
			eng = &entries[i]
			break
		}
	}
	if eng != nil {
		if parent, _ := eng.Data["session"].(string); parent != "" && parent != r.sid {
			a.fork = forkAt(entries, parent)
		}
	} else if hasTurns(entries) {
		a.seed, a.seeded = seedText(entries), "loop"
	}
	if _, err := os.Stat(r.StorePath()); err != nil {
		return nil // built at the first input
	}
	if _, err := r.store.Resume(ctx, session.ID(r.sid)); err != nil {
		// Unreadable (a format bump, a torn file): keep it beside for a
		// person to look at, and reseed from bough history, which is the
		// record. The harness store is a cache that can be rebuilt.
		aside := r.StorePath() + ".unresumable-" + time.Now().UTC().Format("20060102T150405")
		_ = os.Rename(r.StorePath(), aside)
		a.seed, a.seeded = seedText(entries), "reseeded"
		a.fork = nil
		return nil
	}
	a.fork = nil
	max := int64(0) // sequences start at 1: a history with no hseq yet takes every row
	for _, e := range entries {
		// A subagent's rows carry its own store's sequences: counted
		// here, one past this store's latest would skip parent rows.
		if strings.HasPrefix(e.Kind, "sub:") {
			continue
		}
		if h, ok := toInt(e.Data["hseq"]); ok && int64(h) > max {
			max = int64(h)
		}
	}
	n, err := r.replay(ctx, max)
	if err != nil {
		return fmt.Errorf("engine-unreal: catch up %s: %w", r.sid, err)
	}
	a.caught = n
	// The process that cancelled the last turn parked its calls (§9.7)
	// and may have exited before the muted request consumed their
	// results. This process starts unparked, and the coordinator built
	// at the next input would send those results on their own, ahead of
	// that input: the model answering an Esc'd call by itself.
	if cancelledLast(entries) {
		r.gate.Park(r.sync.calls(), nil)
	}
	return nil
}

// cancelledLast: the last turn in history closed as cancelled, by Esc,
// SIGINT, or history closing a turn a dead process left open.
func cancelledLast(entries []history.Entry) bool {
	for i := len(entries) - 1; i >= 0; i-- {
		switch e := entries[i]; e.Kind {
		case "cancelled":
			return true
		case "call":
			// A cancelled call that reported after the 1s wait lands
			// after the close (§9.7 step 3); it is still that turn's.
			if e.Data["canceled"] != true {
				return false
			}
		case "input", "assistant", "error", "system":
			return false
		}
	}
	return false
}

// replay seeds both mirrors and the projector from every store item.
// Items past after (the highest hseq history holds) are projected in
// catch-up mode: content rows recorded, no events, no turn structure —
// history's own open already closed an interrupted turn. A kill -9
// between a store append and a history write loses nothing and writes
// nothing twice. after < 0 records nothing.
func (r *Runtime) replay(ctx context.Context, after int64) (int, error) {
	a := &r.a
	n := 0
	cur := sessionstore.BeforeFirst
	for {
		page, err := r.store.Items(ctx, session.ID(r.sid), cur, 256)
		if err != nil {
			return n, err
		}
		for _, it := range page.Items {
			r.sync.apply(it)
			a.m.Apply(it)
			for _, o := range a.proj.Item(it) {
				if o.Record && after >= 0 && int64(it.Sequence) > after {
					r.d.History.Append(o.Kind, o.Data)
					n++
				}
			}
		}
		if !page.More || page.NextAfter <= cur {
			return n, nil
		}
		cur = page.NextAfter
	}
}

func hasTurns(entries []history.Entry) bool {
	for _, e := range entries {
		if e.Kind == "input" {
			return true
		}
	}
	return false
}

// seedText is an earlier session as the first input's prefix: lossy by
// design, because bough history is the truth and the harness store can
// be rebuilt from it.
func seedText(entries []history.Entry) string {
	var b strings.Builder
	for _, m := range loop.DefaultProject(entries) {
		fmt.Fprintf(&b, "[%s]\n%s\n\n", m.Role, strings.TrimSpace(m.Content))
	}
	body := strings.TrimSpace(b.String())
	if body == "" {
		return ""
	}
	note := ""
	if len(body) > seedCap {
		cut := len(body) - seedCap
		for cut < len(body) && body[cut]&0xC0 == 0x80 {
			cut++
		}
		body = body[cut:]
		note = "(the start of the earlier session was cut; this is its last 400 KiB)\n"
	}
	return "<earlier-session>\nThis session continues an earlier one run by another engine. Its transcript, for context only:\n" + note + body + "\n</earlier-session>"
}

// forkAt reads the fork point from a history file history.Fork wrote:
// the parent's engine entry came along, and the last done is the turn
// forked at.
func forkAt(entries []history.Entry, parent string) *forkPoint {
	fp := &forkPoint{parent: parent}
	start := -1
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if e.Kind == "done" && fp.turn == "" {
			fp.turn, _ = e.Data["engine_turn"].(string)
			fp.running, _ = toInt(e.Data["running"])
			start = i
			continue
		}
		if start >= 0 && e.Kind == "input" {
			if st, _ := e.Data["steer"].(bool); !st {
				var jobs []string
				for _, j := range entries[i:start] {
					if ev, _ := j.Data["event"].(string); j.Kind == "job" && ev == "started" {
						id, _ := toInt(j.Data["id"])
						cmd, _ := j.Data["cmd"].(string)
						jobs = append(jobs, fmt.Sprintf("job %d: %s", id, cmd))
					}
				}
				fp.jobs = strings.Join(jobs, ", ")
				break
			}
		}
	}
	return fp
}

// ensureRun builds the coordinator if there is none (§2 "Coordinator
// build"): the store session, the inbox, the frozen prompt, the tool
// registry snapshot, the builder, then Run on its own goroutine.
func (a *actorState) ensureRun() error {
	if a.run != nil {
		return nil
	}
	r := a.r
	ctx := r.ctx
	sid := session.ID(r.sid)
	restored, err := r.store.Resume(ctx, sid)
	var forkedFrom *forkPoint
	if errors.Is(err, fs.ErrNotExist) {
		if fp := a.fork; fp != nil {
			if fp.running > 0 {
				what := fp.jobs
				if what == "" {
					what = fmt.Sprintf("%d still running", fp.running)
				}
				return fmt.Errorf("engine-unreal: cannot fork at a turn whose calls were still running (%s); fork from the turn before", what)
			}
			if fp.turn == "" {
				return errors.New("engine-unreal: cannot fork here: the turn has no engine_turn to fork the harness session at")
			}
			if _, err := r.store.Fork(ctx, sid, session.ID(fp.parent), session.TurnID(fp.turn)); err != nil {
				return fmt.Errorf("engine-unreal: fork %s at %s: %w", fp.parent, fp.turn, err)
			}
			copyFile(r.systemFile(fp.parent), r.systemFile(r.sid))
			copyFile(r.partsFile(fp.parent), r.partsFile(r.sid))
			if _, err := r.replay(ctx, -1); err != nil {
				return fmt.Errorf("engine-unreal: read forked session: %w", err)
			}
			forkedFrom = fp
			a.fork = nil
		} else if _, err := r.store.Create(ctx, sid); err != nil {
			return fmt.Errorf("engine-unreal: create session %s: %w", r.sid, err)
		}
		restored, err = r.store.Resume(ctx, sid)
	}
	if err != nil {
		return fmt.Errorf("engine-unreal: resume session %s: %w", r.sid, err)
	}

	system, err := a.frozenPrompt()
	if err != nil {
		return err
	}
	snap := r.snapshot()
	reg := r.testReg
	if reg == nil {
		dir := r.d.Cwd
		if r.d.Orb != nil {
			if root, ok := r.d.Orb(); ok && root != "" {
				dir = root
			}
		}
		reg = toolreg.New(toolreg.Config{
			Tools:     snap,
			ViewImage: r.viewImage(dir),
			MaxOutput: r.cfg.MaxOutput,
		})
	}
	model, provider := r.gate.Model()
	b := prompt.Wrap(contextbuilder.NewBuilder(), system, prompt.Placeholder)
	b.SetModel(ullm.Model{ID: orDefault(model, "engine")})
	for _, def := range reg.StaticDefinitions() {
		b.AddTool(def.Tool)
	}

	runCtx, cancel := context.WithCancel(ctx)
	in, err := inbox.New(runCtx, restored.ExternalInputIDs)
	if err != nil {
		cancel()
		return fmt.Errorf("engine-unreal: inbox: %w", err)
	}
	c := coordinator.New(coordinator.Dependencies{
		ToolHeartbeatInterval: r.cfg.Heartbeat,
		SessionID:             sid,
		Inbox:                 in,
		Restored:              restored,
		Sessions:              r.sessions(ctx, sid),
		ContextBuilder:        b,
		LLM:                   r.gate,
		Tools:                 reg,
		Operations:            r.ops,
	})
	a.gen++
	hash := toolreg.Hash(snap)
	a.run = &coord{gen: a.gen, cancel: cancel, inbox: in, hash: hash}

	sum := sha256.Sum256([]byte(system))
	data := map[string]any{
		"engine": "unreal", "pin": unreal.Pin, "sha": unreal.PinSHA[:8], "session": r.sid,
		"store": r.StorePath(), "format": storeFormat, "system": hex.EncodeToString(sum[:]),
		"system_file": r.systemFile(r.sid), "provider": provider, "model": model, "tools": hash,
	}
	if ttl := a.cacheTTL(); ttl > 0 {
		data["cache_ttl"] = int(ttl.Seconds())
	}
	if forkedFrom != nil {
		data["forked_from"], data["fork_turn"] = forkedFrom.parent, forkedFrom.turn
	}
	if a.seeded != "" {
		data["seeded"] = a.seeded
		a.seeded = ""
	}
	// Quiet: bookkeeping for resume, fork and `bough engine`, no event.
	r.d.History.Append("engine", data)
	r.setContext(a.describe(system, hash, model, provider))

	gen := a.gen
	go func() {
		err := c.Run(runCtx)
		cancel() // the harness asks its caller to cancel the Inbox when Run returns
		r.post(func() { a.runExit(gen, err) })
	}()
	// Inputs the last coordinator never recorded were lost with its
	// inbox; the new one gets them again.
	seen := map[string]bool{}
	for _, id := range restored.ExternalInputIDs {
		seen[string(id)] = true
	}
	for id, q := range a.unobserved {
		if !seen[id] {
			a.deliver(q)
		}
	}
	return nil
}

func orDefault(s, d string) string {
	if s == "" {
		return d
	}
	return s
}

func (r *Runtime) systemFile(sid string) string {
	return filepath.Join(r.d.Store, sid+".system.md")
}

func (r *Runtime) partsFile(sid string) string {
	return filepath.Join(r.d.Store, sid+".parts.json")
}

func copyFile(src, dst string) {
	b, err := os.ReadFile(src)
	if err != nil {
		return
	}
	if _, err := os.Stat(dst); err == nil {
		return
	}
	_ = os.WriteFile(dst, b, 0o600)
}

// parts is what the model is told before the conversation, now.
func (a *actorState) parts() prompt.Parts {
	r := a.r
	var p prompt.Parts
	if r.d.Prompt != nil {
		p = r.d.Prompt()
	}
	p.Preamble = r.cfg.SystemPrompt
	if p.Preamble == "" {
		p.Preamble = prompt.Preamble(r.cfg.Heartbeat)
	}
	p.Guidance = r.cfg.TaskGuidance
	return p
}

// frozenPrompt is the system text for every coordinator of this
// session: composed once, written to <sid>.system.md, and read back byte
// for byte afterwards. A mid-session edit of Input[0] would break the
// prompt cache and, on Opus 5.5 / Fable 5.1, the preserved-thinking
// check; drift reaches the model as a <context-update> instead.
func (a *actorState) frozenPrompt() (string, error) {
	r := a.r
	path := r.systemFile(r.sid)
	if b, err := os.ReadFile(path); err == nil {
		a.frozen = string(b)
		if a.lastParts == nil {
			a.lastParts = readParts(r.partsFile(r.sid))
		}
		return a.frozen, nil
	}
	p := a.parts()
	if p.Env == "" {
		p.Env = prompt.Env(r.d.Cwd, time.Now())
	}
	if lc := a.lifecycle(); lc != nil {
		p.SessionStart = lc.SessionStart(r.ctx)
		a.drainHooks()
		if p.SessionStart != "" {
			a.live("context", "hook session-start added context\n"+p.SessionStart, nil)
		}
	}
	system := prompt.Compose(p)
	if err := os.MkdirAll(r.d.Store, 0o700); err != nil {
		return "", fmt.Errorf("engine-unreal: store dir: %w", err)
	}
	if err := os.WriteFile(path, []byte(system), 0o600); err != nil {
		return "", fmt.Errorf("engine-unreal: write %s: %w", path, err)
	}
	a.frozen = system
	a.lastParts = &p
	writeParts(r.partsFile(r.sid), p)
	return system, nil
}

// reminder is the drift since what the model last saw, as a
// <context-update> block for the next input (§11.2). The frozen pieces
// (preamble, env, session-start) are carried over so they never read as
// drift.
func (a *actorState) reminder() string {
	if a.lastParts == nil {
		return ""
	}
	now := a.parts()
	now.Preamble, now.Env, now.SessionStart = a.lastParts.Preamble, a.lastParts.Env, a.lastParts.SessionStart
	text, changed := prompt.Reminder(*a.lastParts, now)
	if text == "" {
		return ""
	}
	for _, c := range changed {
		a.live("context", "context update: "+c, nil)
	}
	a.lastParts = &now
	writeParts(a.r.partsFile(a.r.sid), now)
	return text
}

func readParts(path string) *prompt.Parts {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var p prompt.Parts
	if json.Unmarshal(b, &p) != nil {
		return nil
	}
	return &p
}

func writeParts(path string, p prompt.Parts) {
	if b, err := json.Marshal(p); err == nil {
		_ = os.WriteFile(path, b, 0o600)
	}
}

func (r *Runtime) setContext(s string) {
	r.ctxMu.Lock()
	r.ctxText = s
	r.ctxMu.Unlock()
}

func (a *actorState) describe(system, hash, model, provider string) string {
	r := a.r
	sum := sha256.Sum256([]byte(system))
	var b strings.Builder
	fmt.Fprintf(&b, "engine: unreal-agent %s (%s)\n", unreal.Pin, unreal.PinSHA[:8])
	fmt.Fprintf(&b, "store: %s\n", r.StorePath())
	fmt.Fprintf(&b, "model: %s via %s\n", orDefault(model, "?"), orDefault(provider, "?"))
	fmt.Fprintf(&b, "system prompt: %s (sha256 %s, frozen for the session)\n", r.systemFile(r.sid), hex.EncodeToString(sum[:8]))
	var names []string
	for _, t := range r.snapshot() {
		names = append(names, t.Name)
	}
	fmt.Fprintf(&b, "tools (%s): %s\n", hash[:min(12, len(hash))], strings.Join(names, ", "))
	b.WriteString("\n" + system)
	return b.String()
}

// contextText is /context: the frozen prompt as sent, plus the drift
// that will go out with the next input.
func (a *actorState) contextText() string {
	r := a.r
	system := a.frozen
	if system == "" {
		if b, err := os.ReadFile(r.systemFile(r.sid)); err == nil {
			system = string(b)
		} else {
			system = prompt.Compose(a.parts()) + "\n\n(not frozen yet: the session's first input freezes it)"
		}
	}
	model, provider := r.gate.Model()
	s := a.describe(system, r.toolsHash(), model, provider)
	if a.lastParts != nil {
		now := a.parts()
		now.Preamble, now.Env, now.SessionStart = a.lastParts.Preamble, a.lastParts.Env, a.lastParts.SessionStart
		if text, _ := prompt.Reminder(*a.lastParts, now); text != "" {
			s += "\n\npending update, sent with your next message:\n" + text
		}
	}
	return s
}

// watch feeds the actor what arrives on channels it cannot select on
// itself: a tool-set change and job-notice wakes. Both services are
// resolved every second, since their rows mount and remount on their
// own schedule.
func (r *Runtime) watch() {
	var tools <-chan struct{}
	if r.d.Tools != nil {
		tools = r.d.Tools.Changed()
	}
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	var wake <-chan struct{}
	resolve := func() {
		wake = nil
		if r.d.Jobs != nil {
			if j := r.d.Jobs(); j != nil {
				wake = j.Wake()
			}
		}
	}
	resolve()
	for {
		select {
		case <-r.ctx.Done():
			return
		case <-tools:
			tools = r.d.Tools.Changed()
			r.post(func() { r.a.toolsChanged() })
		case <-wake:
			r.post(func() { r.a.notice() })
		case <-tick.C:
			resolve()
		}
	}
}
