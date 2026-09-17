package serve

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

// StatusQueued is a background agent waiting for a running slot. It is
// declared here, not with the derived statuses: it is never read from
// history, only from the supervisor's in-memory queue.
const StatusQueued Status = "queued"

var (
	// ErrDepth refuses a background agent starting agents of its own.
	ErrDepth = errors.New("serve: supervisor: a background agent cannot start agents (depth 1)")
	// ErrAgentLimit is a parent past its lifetime agent budget.
	ErrAgentLimit = errors.New("serve: supervisor: background agent limit reached")
)

const (
	defaultMaxPerSession = 200
	defaultMaxRunning    = 16
	reportRunes          = 2000
	reportWait           = 2 * time.Second
)

// queuedChild is everything needed to start a child later: its id is
// minted at create time so the parent gets an answer at once.
type queuedChild struct {
	id, dir, prompt string
	extra, args     []string
}

// ChildTask is a queued child's start as meta.json keeps it.
type ChildTask struct {
	Dir    string   `json:"dir,omitempty"`
	Prompt string   `json:"prompt,omitempty"`
	Extra  []string `json:"extra,omitempty"`
	Args   []string `json:"args,omitempty"`
}

// requeueLocked rebuilds the queue from persisted tasks, oldest first
// (ids are time-ordered). Caller holds s.mu or owns s alone.
func (s *Supervisor) requeueLocked() {
	var ids []string
	for id, m := range s.meta {
		if m.Task != nil {
			ids = append(ids, id)
		}
	}
	sort.Strings(ids)
	for _, id := range ids {
		m := s.meta[id]
		m.Queued = true
		s.meta[id] = m
		s.queue = append(s.queue, queuedChild{id: id, dir: m.Task.Dir, prompt: m.Task.Prompt, extra: m.Task.Extra, args: m.Task.Args})
	}
}

// ChildInfo is one background agent as its parent sees it.
type ChildInfo struct {
	ID, Parent, Title, Project string
	Queued                     bool
	Status                     Status // derived; "" when queued
	Error                      string // failed last turn: its first error line, cleaned
}

// CreateChild starts (or queues) a background agent for opt.SpawnedBy.
// The child inherits the parent's mode unless opt.Slug names a project:
// a local parent handing work to a project orb is allowed without a
// person saying so, since the orb is where writes belong.
func (s *Supervisor) CreateChild(opt CreateOptions, maxPerSession, maxRunning int) (id string, queued bool, err error) {
	parent := opt.SpawnedBy
	if maxPerSession <= 0 {
		maxPerSession = defaultMaxPerSession
	}
	if maxRunning <= 0 {
		maxRunning = defaultMaxRunning
	}
	pinfo, ok := s.infoOf(parent)
	if !ok {
		return "", false, fmt.Errorf("serve: supervisor: parent %s: %w", parent, ErrUnknownSession)
	}
	if pinfo.SpawnedBy != "" || s.Meta(parent).SpawnedBy != "" {
		return "", false, ErrDepth
	}
	q := queuedChild{id: history.NewID(), prompt: opt.Prompt}
	// The parent's model, as the loop pipeline pins one: the child's
	// config default may be a provider with no key here.
	if plugin, name, ok := strings.Cut(opt.Model, "/"); ok && plugin != "" && name != "" {
		q.args = []string{"--set", "llm.plugin=" + plugin, "--set", "llm.model=" + name}
	}
	slug := opt.Slug
	if slug == "" && pinfo.Mode == "project" {
		slug = pinfo.Project
	}
	if slug != "" {
		if err := projectdef.ValidSlug(slug); err != nil {
			return "", false, fmt.Errorf("serve: supervisor: background agent: %w", err)
		}
		// Same as Create: the child builds its orb and chdirs itself.
		q.dir = s.home
		q.extra = []string{"BOUGH_MODE=project", "BOUGH_PROJECT=" + slug}
	} else {
		q.dir = firstDir(pinfo.Cwd, opt.Cwd, s.cwd)
	}
	// The child's history row names its file after this id and records
	// the parent; cmd/bough clears both vars so the child's own shell
	// commands never inherit them.
	q.extra = append(q.extra, "BOUGH_SESSION_ID="+q.id, "BOUGH_SPAWNED_BY="+parent)

	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return "", false, fmt.Errorf("serve: supervisor: closed")
	}
	n := 0
	for _, m := range s.meta {
		if m.SpawnedBy == parent {
			n++
		}
	}
	if n >= maxPerSession {
		s.mu.Unlock()
		return "", false, fmt.Errorf("%w (%d per session)", ErrAgentLimit, maxPerSession)
	}
	// The last request's cap wins: every session reads the same bough.yml
	// in practice, and a lowered setting should take effect.
	s.maxRunning = maxRunning
	m := SessionMeta{SpawnedBy: parent}
	if slug != "" {
		for _, p := range s.projects {
			if p.Slug == slug {
				m.Project = p.ID
			}
		}
	}
	if len(s.running) >= s.maxRunning {
		m.Queued = true
		m.Task = &ChildTask{Dir: q.dir, Prompt: q.prompt, Extra: q.extra, Args: q.args}
		s.meta[q.id] = m
		s.queue = append(s.queue, q)
		err := s.saveMetaLocked()
		s.mu.Unlock()
		return q.id, true, err
	}
	s.meta[q.id] = m
	// Counted from the moment of start, not from the first derived
	// running status: a burst of spawns would otherwise all see zero
	// running while their processes boot.
	s.running[q.id] = true
	ch := newChild(q.id)
	ch.booting = true
	s.kids[q.id] = ch
	if err := s.saveMetaLocked(); err != nil {
		s.mu.Unlock()
		s.abandon(ch)
		return "", false, err
	}
	s.mu.Unlock()
	if err := s.launch(ch, q); err != nil {
		return "", false, err
	}
	return q.id, false, nil
}

func firstDir(dirs ...string) string {
	for _, d := range dirs {
		if d != "" {
			return d
		}
	}
	return ""
}

// launch starts a reserved child and hands it its task. A stop that
// arrived while the process booted wins: the child is killed before it
// ever reads its task, since SIGINT to an idle process cancels nothing
// and the prompt would then run in full.
func (s *Supervisor) launch(ch *child, q queuedChild) error {
	if len(q.args) > 0 {
		// A respawn through ensure keeps the model.
		s.mu.Lock()
		if s.spawnArgs == nil {
			s.spawnArgs = map[string]spawnSpec{}
		}
		s.spawnArgs[q.id] = spawnSpec{args: slices.Clone(q.args)}
		s.mu.Unlock()
	}
	if err := s.start(ch, q.dir, "", q.extra, q.args); err != nil {
		s.abandon(ch)
		return err
	}
	s.mu.Lock()
	stop := ch.stopReq
	ch.booting = false
	s.mu.Unlock()
	if stop {
		s.killChild(ch)
		return nil
	}
	if q.prompt != "" {
		if err := s.writePrompt(ch, q.prompt); err != nil {
			return err
		}
	}
	return nil
}

// abandon undoes a reservation whose process never started, freeing
// its slot for the next queued child.
func (s *Supervisor) abandon(ch *child) {
	s.mu.Lock()
	if s.kids[ch.id] == ch {
		delete(s.kids, ch.id)
	}
	delete(s.running, ch.id)
	s.mu.Unlock()
	close(ch.done)
	go s.drainQueue()
}

// drainQueue starts queued children while slots are free, oldest first.
// Each is popped under s.mu before it is started, so two drains cannot
// start the same child.
func (s *Supervisor) drainQueue() {
	for {
		s.mu.Lock()
		max := s.maxRunning
		if max <= 0 {
			max = defaultMaxRunning
		}
		if s.closed || len(s.queue) == 0 || len(s.running) >= max {
			s.mu.Unlock()
			return
		}
		q := s.queue[0]
		s.queue = s.queue[1:]
		m := s.meta[q.id]
		m.Queued = false
		m.Task = nil
		s.meta[q.id] = m
		s.running[q.id] = true
		ch := newChild(q.id)
		ch.booting = true
		s.kids[q.id] = ch
		// Saved before launch: a restart after this start must not
		// start the same child a second time.
		serr := s.saveMetaLocked()
		s.mu.Unlock()
		if serr != nil {
			s.mu.Lock()
			s.emitLocked(q.id, "error", serr.Error(), nil)
			s.mu.Unlock()
		}
		if err := s.launch(ch, q); err != nil {
			s.mu.Lock()
			s.emitLocked(q.id, "error", err.Error(), nil)
			s.mu.Unlock()
		}
	}
}

// childEventLocked keeps the running count and triggers the report.
// Caller holds s.mu.
func (s *Supervisor) childEventLocked(id, kind string) {
	m, ok := s.meta[id]
	if !ok || m.SpawnedBy == "" {
		return
	}
	switch kind {
	case "input":
		s.running[id] = true
	case "done", "cancelled", "exit":
		// Not "error": the loop writes it MID-turn, several per turn.
		delete(s.running, id)
		go s.report(id, m.SpawnedBy, kind)
		go s.drainQueue()
	}
}

// turnEnd is the last turn of a session file as the report needs it.
type turnEnd struct {
	closing  *history.Entry // nil while the last turn is open
	input    *history.Entry // the input that opened the last turn, if any
	errored  bool
	errText  string // the turn's first error entry
	reply    string
	hasEntry bool
}

// lastTurn mirrors StatusOf's turn scan: input opens, done/cancelled
// close, and a done right after a cancel is bookkeeping, not a turn.
func lastTurn(entries []history.Entry) turnEnd {
	var t turnEnd
	open, lastClose := false, ""
	for i := range entries {
		e := &entries[i]
		switch e.Kind {
		case "input":
			t = turnEnd{input: e, hasEntry: true}
			open, lastClose = true, ""
		case "error":
			if !t.errored {
				t.errText, _ = e.Data["text"].(string)
			}
			t.errored = true
		case "assistant":
			if txt, _ := e.Data["text"].(string); txt != "" {
				t.reply = txt
			}
		case "done", "cancelled":
			if e.Kind == "done" && !open && lastClose == "cancelled" {
				continue
			}
			t.closing, t.hasEntry = e, true
			open, lastClose = false, e.Kind
		}
	}
	return t
}

// LastTurn is the last turn's reply and whether it errored, for callers
// outside serve (the loop runner) that drive a child turn by turn.
func LastTurn(entries []history.Entry) (reply string, errored bool) {
	t := lastTurn(entries)
	return t.reply, t.errored
}

// report tells the parent a child's turn ended, exactly once per turn.
// The event is only the trigger: the closing entry on disk decides, so
// the reply the parent reads is the one the child actually recorded.
func (s *Supervisor) report(id, parent, trigger string) {
	deadline := time.Now().Add(reportWait)
	var key int64
	var word string
	var t turnEnd
	for {
		entries, err := s.Entries(id)
		if err != nil && trigger != "exit" {
			return
		}
		t = lastTurn(entries)
		closed := t.closing != nil && (t.input == nil || t.closing.Seq > t.input.Seq)
		switch {
		case closed:
			key = t.closing.Seq
			word = "finished"
			if t.closing.Kind == "cancelled" {
				word = "stopped"
			} else if t.errored {
				word = "failed"
			}
		case t.input != nil && (trigger == "exit" || time.Now().After(deadline)):
			// The process died mid-turn, or the flush never came: report
			// from what is there, keyed on the turn's input.
			key = t.input.Seq
			word = map[string]string{"done": "finished", "cancelled": "stopped", "exit": "stopped"}[trigger]
		case time.Now().After(deadline) || !t.hasEntry:
			return
		default:
			time.Sleep(createPoll)
			continue
		}
		break
	}
	s.mu.Lock()
	m, ok := s.meta[id]
	if !ok || key <= m.Reported {
		s.mu.Unlock()
		return
	}
	m.Reported = key
	s.meta[id] = m
	// Best effort: a failed save risks one duplicate after a restart,
	// which beats withholding the report.
	_ = s.saveMetaLocked()
	s.mu.Unlock()
	// A parent that cannot be told (deleted file) has nobody to tell.
	_ = s.notifyFrom(parent, id, reportText(id, s.childTitle(id), word, failText(word, t)))
}

// failText is the notice body: a failure leads with its reason, since
// "failed" alone gave the parent nothing to act on.
func failText(word string, t turnEnd) string {
	if word != "failed" {
		return t.reply
	}
	r := failReason(t.errText)
	if r == "" {
		return t.reply
	}
	out := "Background agent failed: " + r
	if t.reply != "" {
		out += "\n" + t.reply
	}
	return out
}

var tempPath = regexp.MustCompile(`(?:/private)?/(?:tmp|var/folders)/\S*/`)

// failReason is an error's first non-blank line without goja's
// "GoError: " prefix or temp directories (noise, and long).
func failReason(text string) string {
	for _, l := range strings.Split(text, "\n") {
		l = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(l), "GoError: "))
		if l != "" {
			return tempPath.ReplaceAllString(l, "")
		}
	}
	return ""
}

func reportText(id, title, word, reply string) string {
	if title == "" {
		title = idTail(id)
	}
	if r := []rune(reply); len(r) > reportRunes {
		reply = string(r[:reportRunes]) + "…" + fmt.Sprintf("\n(tools.agent(%q) has the full reply)", id)
	}
	return fmt.Sprintf("[agent %s · %s %s] %s", title, id, word, reply)
}

func idTail(id string) string {
	if len(id) > 6 {
		return id[len(id)-6:]
	}
	return id
}

func (s *Supervisor) childTitle(id string) string {
	if t := s.Meta(id).Title; t != "" {
		return t
	}
	if in, ok := s.infoOf(id); ok {
		return in.Title
	}
	return ""
}

func (s *Supervisor) infoOf(id string) (history.SessionInfo, bool) {
	if id == "" {
		return history.SessionInfo{}, false
	}
	infos, err := history.List(s.opt.HistDir)
	if err != nil {
		return history.SessionInfo{}, false
	}
	for _, in := range infos {
		if in.ID == id {
			return in, true
		}
	}
	return history.SessionInfo{}, false
}

// Children lists a session's background agents, oldest first (ids are
// time-ordered).
func (s *Supervisor) Children(parent string) []ChildInfo {
	s.mu.Lock()
	var out []ChildInfo
	for id, m := range s.meta {
		if m.SpawnedBy == parent {
			out = append(out, ChildInfo{ID: id, Parent: parent, Title: m.Title, Queued: m.Queued})
		}
	}
	s.mu.Unlock()
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	for i := range out {
		c := &out[i]
		if c.Queued {
			continue
		}
		entries, _ := s.Entries(c.ID)
		c.Status, _ = StatusOf(entries, s.Live(c.ID))
		if t := lastTurn(entries); t.errored {
			c.Error = failReason(t.errText)
		}
		_, c.Project = sessionMode(entries)
		if c.Title == "" {
			c.Title = s.childTitle(c.ID)
		}
	}
	return out
}

// QueuedPrompt is the task of a queued child, "" when it is not queued.
func (s *Supervisor) QueuedPrompt(id string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, q := range s.queue {
		if q.id == id {
			return q.prompt, true
		}
	}
	return "", false
}

// agentCounts is what a parent row carries: running children, queued
// ones, and every child it ever started.
func (s *Supervisor) agentCounts(parent string) (running, queued, total int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, m := range s.meta {
		if m.SpawnedBy != parent {
			continue
		}
		total++
		switch {
		case m.Queued:
			queued++
		case s.running[id]:
			running++
		}
	}
	return running, queued, total
}

// queuedIDs are the children waiting for a slot, oldest first.
func (s *Supervisor) queuedIDs() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]string, len(s.queue))
	for i, q := range s.queue {
		out[i] = q.id
	}
	return out
}

// EndChild is archive's stop: it drops a queued child and kills a live
// one whether it is mid-turn or idle, and stops a project child's orb.
// stopChild's interrupt keeps the process by design, which under an
// archived parent would orphan it and its container.
func (s *Supervisor) EndChild(id string) error {
	if was, err := s.stopChild(id); err != nil || was == "queued" {
		return err
	}
	if err := s.Kill(id); err != nil {
		return err
	}
	entries, _ := s.Entries(id)
	if mode, _ := sessionMode(entries); mode != "project" {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	name := container.OrbName(id)
	if st, err := s.rt.Inspect(ctx, name); err != nil || st != container.StateRunning {
		return nil // no container, or already down: nothing to stop
	}
	if err := s.rt.Stop(ctx, name); err != nil {
		return fmt.Errorf("serve: supervisor: stop orb of %s: %w", id, err)
	}
	s.mu.Lock()
	s.stoppedAt[id] = time.Now()
	s.mu.Unlock()
	return nil
}

// stopChild reports what the child was: "queued", "running" or "idle".
// A dropped queued child never ran, so it gives its slot in the
// parent's lifetime budget back and leaves no row behind.
func (s *Supervisor) stopChild(id string) (string, error) {
	s.mu.Lock()
	for i, q := range s.queue {
		if q.id == id {
			s.queue = append(s.queue[:i:i], s.queue[i+1:]...)
			delete(s.meta, id)
			err := s.saveMetaLocked()
			s.mu.Unlock()
			return "queued", err
		}
	}
	running := s.running[id]
	ch, live := s.kids[id]
	if running && live && ch.booting {
		// Still booting: launch sees this and kills instead of prompting.
		ch.stopReq = true
		s.mu.Unlock()
		return "running", nil
	}
	s.mu.Unlock()
	if !running || !live {
		return "idle", nil
	}
	if err := s.Interrupt(id); err != nil {
		return "", err
	}
	return "running", nil
}

// oneLineTitle is a queued child's row title: its task's first line.
func oneLineTitle(prompt string) string {
	if i := strings.IndexByte(prompt, '\n'); i >= 0 {
		prompt = prompt[:i]
	}
	return prompt
}
