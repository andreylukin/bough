// Supervisor owns bough sessions as child processes: one
// `bough --headless --json` per session, never two. history's
// ConcurrentWriter exists because two processes appending one session
// file is a real hazard, so the lease map here is the whole point of
// the package — everything else (the event ring, the fan-out, the
// archive/title store) hangs off it.
package serve

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Event is one line of a child's output, or a synthetic line the
// supervisor makes up for things the child cannot say (a non-JSON
// stdout line, its own exit).
type Event struct {
	Session string         `json:"session"`
	Seq     int64          `json:"seq"` // supervisor-assigned, per session, from 1
	At      time.Time      `json:"at"`
	Kind    string         `json:"kind"` // verbatim from the child's JSON "kind"
	Text    string         `json:"text"`
	Extra   map[string]any `json:"extra,omitempty"` // every other key of the child's line
}

// SessionMeta is the supervisor's own metadata for a session. It is
// never written into history and never holds a status: status is
// derived from the entries (see StatusOf), so it cannot go stale.
type SessionMeta struct {
	Title    string `json:"title,omitempty"` // supervisor-set rename; "" = use history title
	Archived bool   `json:"archived,omitempty"`
	// Model and Effort are what this supervisor last ASKED a session
	// for, not necessarily what the child is running: the child owns
	// its config, and a resumed session starts from bough.yml. "" means
	// never set from here, which the UI shows as the default.
	Model  string `json:"model,omitempty"`
	Effort string `json:"effort,omitempty"`
	// Project is the grouping this session belongs to, by project id.
	// "" is ungrouped, which is the normal state — a session is never
	// forced into one.
	Project string `json:"project,omitempty"`
}

// Project is a named grouping of sessions. It exists independently of
// its members so an empty project can be created first and filled
// later, and so renaming one does not touch any session.
type Project struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// Options configures a Supervisor. Every path is explicit so tests can
// run against a t.TempDir() HOME.
type Options struct {
	Exe      string   // bough binary; "" => os.Executable()
	HistDir  string   // $HOME/.bough/history
	MetaPath string   // $HOME/.bough/serve/meta.json
	Env      []string // child env; nil => os.Environ()
	Buffer   int      // per-session event ring; <=0 => 500
}

var (
	ErrNoAsk          = errors.New("serve: supervisor: no pending ask")
	ErrUnknownSession = errors.New("serve: supervisor: unknown session")
	ErrArchived       = errors.New("serve: supervisor: session is archived")
)

const (
	defaultRing   = 500
	subBuffer     = 256 // a subscriber this far behind is dropped, not waited for
	createTimeout = 10 * time.Second
	createPoll    = 50 * time.Millisecond
)

// pending is an event emitted before its child's session id is known
// (the window between spawn and id discovery in Create).
type pending struct {
	kind  string
	text  string
	extra map[string]any
}

type child struct {
	cmd   *exec.Cmd
	stdin io.WriteCloser
	inMu  sync.Mutex // one writer at a time: a torn line would be read as two prompts

	done chan struct{} // closed once the process is reaped and the lease dropped

	// Guarded by Supervisor.mu.
	id      string
	metaID  string // a session id the child volunteered on a "meta" line
	buffer  []pending
	dropped bool // the lease has already been handed back
}

// Supervisor is safe for concurrent use. One mutex guards the lease
// map, the event rings, the subscribers and the meta store: they are
// mutated together often enough that splitting them would buy nothing
// but lock-ordering bugs.
type Supervisor struct {
	opt  Options
	exe  string
	cwd  string
	ring int

	// createMu serializes Create so two callers cannot both claim the
	// same freshly-appeared history id.
	createMu sync.Mutex

	mu       sync.Mutex
	kids     map[string]*child
	events   map[string][]Event
	seq      map[string]int64
	asks     map[string]*Ask
	subs     map[string]map[int]chan Event
	nextID   int
	meta     map[string]SessionMeta
	projects map[string]Project
	closed   bool
}

// NewSupervisor loads the meta store and resolves the bough binary. A
// missing meta.json is an empty store, not an error.
func NewSupervisor(opt Options) (*Supervisor, error) {
	exe := opt.Exe
	if exe == "" {
		p, err := os.Executable()
		if err != nil {
			return nil, fmt.Errorf("serve: supervisor: locate bough: %w", err)
		}
		exe = p
	}
	cwd, err := os.Getwd()
	if err != nil {
		return nil, fmt.Errorf("serve: supervisor: working directory: %w", err)
	}
	ring := opt.Buffer
	if ring <= 0 {
		ring = defaultRing
	}
	s := &Supervisor{
		opt:      opt,
		exe:      exe,
		cwd:      cwd,
		ring:     ring,
		kids:     map[string]*child{},
		events:   map[string][]Event{},
		seq:      map[string]int64{},
		asks:     map[string]*Ask{},
		subs:     map[string]map[int]chan Event{},
		meta:     map[string]SessionMeta{},
		projects: map[string]Project{},
	}
	if opt.MetaPath != "" {
		if err := os.MkdirAll(filepath.Dir(opt.MetaPath), 0o755); err != nil {
			return nil, fmt.Errorf("serve: supervisor: meta dir: %w", err)
		}
		if err := s.loadMeta(); err != nil {
			return nil, err
		}
	}
	return s, nil
}

func (s *Supervisor) HistDir() string { return s.opt.HistDir }

// Entries reads one session's history. An id with no file is
// ErrUnknownSession, so callers can answer 404 without statting.
func (s *Supervisor) Entries(id string) ([]history.Entry, error) {
	entries, err := history.Read(filepath.Join(s.opt.HistDir, id+".jsonl"))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, fmt.Errorf("serve: supervisor: %s: %w", id, ErrUnknownSession)
		}
		return nil, fmt.Errorf("serve: supervisor: read %s: %w", id, err)
	}
	return entries, nil
}

func (s *Supervisor) List() ([]history.SessionInfo, error) {
	infos, err := history.List(s.opt.HistDir)
	if err != nil {
		return nil, fmt.Errorf("serve: supervisor: list sessions: %w", err)
	}
	return infos, nil
}

// Create spawns a fresh headless session in cwd and, once its history
// id appears, writes prompt as its first line.
//
// The child picks its own id, so discovery is a diff of history.List
// before and after the spawn, bounded by createTimeout. createMu keeps
// two Creates from racing over one new file.
func (s *Supervisor) Create(cwd, prompt string) (string, error) {
	s.createMu.Lock()
	defer s.createMu.Unlock()

	before := map[string]bool{}
	infos, err := s.List()
	if err != nil {
		return "", err
	}
	for _, in := range infos {
		before[in.ID] = true
	}

	ch, err := s.spawn(cwd, "")
	if err != nil {
		return "", err
	}

	deadline := time.Now().Add(createTimeout)
	for {
		s.mu.Lock()
		id := ch.metaID // the child's own word for it beats a directory diff
		s.mu.Unlock()
		if id == "" {
			id = s.newSessionID(before)
		}
		if id != "" {
			if err := s.claim(ch, id); err != nil {
				s.killChild(ch)
				return "", err
			}
			if prompt != "" {
				if err := s.write(ch, prompt); err != nil {
					return id, err
				}
			}
			return id, nil
		}
		select {
		case <-ch.done:
			return "", fmt.Errorf("serve: supervisor: session exited before writing history")
		default:
		}
		if time.Now().After(deadline) {
			s.killChild(ch)
			return "", fmt.Errorf("serve: supervisor: no new session id in %s under %s", createTimeout, s.opt.HistDir)
		}
		time.Sleep(createPoll)
	}
}

// newSessionID returns the one id in HistDir that is not in before,
// and "" when there is none or more than one (an unrelated bough
// started by the user at that moment must not be adopted).
func (s *Supervisor) newSessionID(before map[string]bool) string {
	infos, err := history.List(s.opt.HistDir)
	if err != nil {
		return ""
	}
	found := ""
	for _, in := range infos {
		if before[in.ID] {
			continue
		}
		if found != "" {
			return ""
		}
		found = in.ID
	}
	return found
}

// claim installs a resolved id on a child and flushes the events it
// emitted while nameless.
func (s *Supervisor) claim(ch *child, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return fmt.Errorf("serve: supervisor: closed")
	}
	if live, ok := s.kids[id]; ok && live != ch {
		return fmt.Errorf("serve: supervisor: %s: %w", id, errors.New("session already leased"))
	}
	ch.id = id
	s.kids[id] = ch
	buffered := ch.buffer
	ch.buffer = nil
	for _, p := range buffered {
		s.emitLocked(id, p.kind, p.text, p.extra)
	}
	return nil
}

// Adopt takes over an existing session by spawning `-r <id>`. A live
// lease is left alone: re-adopting would mean two writers.
func (s *Supervisor) Adopt(id string) error {
	if s.Meta(id).Archived {
		return fmt.Errorf("serve: supervisor: %s: %w", id, ErrArchived)
	}
	_, err := s.ensure(id)
	return err
}

// ensure returns the session's live child, spawning one if the lease
// is free. The map check and the spawn happen under one mutex hold:
// check-then-spawn would let two children share a session file.
func (s *Supervisor) ensure(id string) (*child, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil, fmt.Errorf("serve: supervisor: closed")
	}
	if ch, ok := s.kids[id]; ok {
		s.mu.Unlock()
		return ch, nil
	}
	// Reserve the lease before releasing the mutex, so a concurrent
	// ensure waits for this spawn rather than starting a second one.
	ch := &child{id: id, done: make(chan struct{})}
	s.kids[id] = ch
	s.mu.Unlock()

	if err := s.start(ch, s.adoptDir(id), id); err != nil {
		s.mu.Lock()
		if s.kids[id] == ch {
			delete(s.kids, id)
		}
		s.mu.Unlock()
		close(ch.done)
		return nil, err
	}
	return ch, nil
}

// adoptDir is the directory the session was started in, so relative
// paths in its transcript keep meaning; the supervisor's own cwd is
// the fallback for sessions written before cwd was recorded.
func (s *Supervisor) adoptDir(id string) string {
	infos, err := history.List(s.opt.HistDir)
	if err != nil {
		return s.cwd
	}
	for _, in := range infos {
		if in.ID == id && in.Cwd != "" {
			if st, err := os.Stat(in.Cwd); err == nil && st.IsDir() {
				return in.Cwd
			}
		}
	}
	return s.cwd
}

// spawn starts a child that has no id yet (Create's case).
func (s *Supervisor) spawn(cwd, id string) (*child, error) {
	ch := &child{done: make(chan struct{})}
	if err := s.start(ch, cwd, id); err != nil {
		close(ch.done)
		return nil, err
	}
	return ch, nil
}

// start builds and launches the process and the goroutines that read
// it. NEVER a bare --resume: headless refuses it, prints the session
// list and exits 2.
func (s *Supervisor) start(ch *child, dir, id string) error {
	args := []string{"--headless", "--json"}
	if id != "" {
		args = append(args, "-r", id)
	}
	cmd := exec.Command(s.exe, args...)
	cmd.Dir = dir
	cmd.Env = s.opt.Env
	if cmd.Env == nil {
		cmd.Env = os.Environ()
	}
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("serve: supervisor: stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("serve: supervisor: stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("serve: supervisor: stderr pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("serve: supervisor: start %s: %w", s.exe, err)
	}
	ch.cmd = cmd
	ch.stdin = stdin

	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); s.pumpStdout(ch, stdout) }()
	go func() { defer wg.Done(); s.pumpStderr(ch, stderr) }()
	go func() {
		// Wait only after both pipes are drained: reaping first closes
		// them under the readers and loses the child's last lines.
		wg.Wait()
		err := cmd.Wait()
		code := 0
		if ee := new(exec.ExitError); errors.As(err, &ee) {
			code = ee.ExitCode()
		} else if err != nil {
			code = -1
		}
		s.emit(ch, "exit", fmt.Sprintf("child exited: %v", err), map[string]any{"code": code})
		s.drop(ch)
		close(ch.done)
	}()
	return nil
}

// pumpStdout turns the child's JSON lines into Events. The buffer
// ceiling matches headlessPump's: a long tool result must not silently
// end the scan and leave the session looking hung forever.
func (s *Supervisor) pumpStdout(ch *child, r io.Reader) {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 1024*1024), 16*1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var obj map[string]any
		if err := json.Unmarshal([]byte(line), &obj); err != nil {
			// A stray print, or a stale binary writing "[kind] text".
			// Surfaced, never dropped.
			s.emit(ch, "stdout", line, nil)
			continue
		}
		kind, _ := obj["kind"].(string)
		text, _ := obj["text"].(string)
		delete(obj, "kind")
		delete(obj, "text")
		var extra map[string]any
		if len(obj) > 0 {
			extra = obj
		}
		if kind == "" {
			kind = "stdout"
		}
		s.emit(ch, kind, text, extra)
	}
	if err := sc.Err(); err != nil {
		s.emit(ch, "error", fmt.Sprintf("serve: supervisor: stdout: %v", err), nil)
	}
}

func (s *Supervisor) pumpStderr(ch *child, r io.Reader) {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 1024*1024), 16*1024*1024)
	for sc.Scan() {
		if line := sc.Text(); line != "" {
			s.emit(ch, "error", line, nil)
		}
	}
	if err := sc.Err(); err != nil {
		s.emit(ch, "error", fmt.Sprintf("serve: supervisor: stderr: %v", err), nil)
	}
}

// emit records an event, or parks it when the child's id is still
// being discovered.
func (s *Supervisor) emit(ch *child, kind, text string, extra map[string]any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if ch.id == "" {
		if kind == "meta" && ch.metaID == "" {
			ch.metaID = metaSession(extra)
		}
		ch.buffer = append(ch.buffer, pending{kind: kind, text: text, extra: extra})
		return
	}
	s.emitLocked(ch.id, kind, text, extra)
}

// metaSession digs a session id out of a "meta" line's extras. The
// current headless build volunteers none, so this is opportunistic:
// Create's directory diff is the load-bearing path.
func metaSession(extra map[string]any) string {
	for _, k := range []string{"session", "session_id", "id"} {
		if v, ok := extra[k].(string); ok && v != "" {
			return v
		}
	}
	if p, ok := extra["path"].(string); ok && p != "" {
		base := filepath.Base(p)
		if ext := filepath.Ext(base); ext == ".jsonl" {
			return base[:len(base)-len(ext)]
		}
	}
	return ""
}

// emitLocked assigns the per-session seq, appends to the ring, tracks
// the armed ask and fans out. Caller holds s.mu.
func (s *Supervisor) emitLocked(id, kind, text string, extra map[string]any) {
	s.seq[id]++
	ev := Event{Session: id, Seq: s.seq[id], At: time.Now(), Kind: kind, Text: text, Extra: extra}

	buf := append(s.events[id], ev)
	if len(buf) > s.ring {
		buf = buf[len(buf)-s.ring:]
	}
	s.events[id] = buf

	switch kind {
	case "ask":
		s.asks[id] = askFrom(ev)
	case "done", "cancelled", "error", "exit":
		// The turn (or the process) ended: stop routing stdin to an
		// ask nobody is waiting on any more.
		delete(s.asks, id)
	}

	for key, cch := range s.subs[id] {
		select {
		case cch <- ev:
		default:
			// A stalled reader must not stall the scanner for every
			// other subscriber: drop it and let it reconnect.
			close(cch)
			delete(s.subs[id], key)
		}
	}
}

func askFrom(ev Event) *Ask {
	a := &Ask{Text: ev.Text, Seq: ev.Seq}
	if id, ok := ev.Extra["id"].(string); ok {
		a.ID = id
	}
	if opts, ok := ev.Extra["options"].([]any); ok {
		for _, o := range opts {
			if s, ok := o.(string); ok {
				a.Options = append(a.Options, s)
			}
		}
	}
	if opts, ok := ev.Extra["options"].([]string); ok {
		a.Options = append(a.Options, opts...)
	}
	return a
}

// drop hands the lease back. It runs before ch.done closes, so a
// respawn can never overlap the process it replaces.
func (s *Supervisor) drop(ch *child) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if ch.dropped {
		return
	}
	ch.dropped = true
	if ch.id != "" && s.kids[ch.id] == ch {
		delete(s.kids, ch.id)
	}
	delete(s.asks, ch.id)
}

// Send writes one line to the session's stdin, adopting it first when
// nothing holds the lease. The child decides whether that line steers
// a running turn or starts a new one.
func (s *Supervisor) Send(id, text string) error {
	if s.Meta(id).Archived {
		return fmt.Errorf("serve: supervisor: %s: %w", id, ErrArchived)
	}
	// An armed ask eats the next stdin line, so a prompt sent now
	// would silently become the answer. Refuse rather than guess.
	if a := s.PendingAsk(id); a != nil {
		return fmt.Errorf("serve: supervisor: %s: a pending ask (%s) takes the next line; answer it first", id, a.Text)
	}
	ch, err := s.ensure(id)
	if err != nil {
		return err
	}
	return s.write(ch, text)
}

// Answer replies to the armed tools.ask over the same pipe.
func (s *Supervisor) Answer(id, text string) error {
	if s.PendingAsk(id) == nil {
		return ErrNoAsk
	}
	s.mu.Lock()
	ch, ok := s.kids[id]
	s.mu.Unlock()
	if !ok {
		// The ask belongs to a process that is gone; nothing can read
		// the answer, and a respawn would re-ask.
		return ErrNoAsk
	}
	if err := s.write(ch, text); err != nil {
		return err
	}
	s.mu.Lock()
	delete(s.asks, id)
	s.mu.Unlock()
	return nil
}

func (s *Supervisor) write(ch *child, text string) error {
	ch.inMu.Lock()
	defer ch.inMu.Unlock()
	if ch.stdin == nil {
		return fmt.Errorf("serve: supervisor: session has no stdin")
	}
	if _, err := io.WriteString(ch.stdin, text+"\n"); err != nil {
		return fmt.Errorf("serve: supervisor: write stdin: %w", err)
	}
	return nil
}

// Interrupt SIGINTs the child and keeps the lease, so the session can
// be prompted again without a respawn.
func (s *Supervisor) Interrupt(id string) error {
	s.mu.Lock()
	ch, ok := s.kids[id]
	s.mu.Unlock()
	if !ok {
		return fmt.Errorf("serve: supervisor: %s: %w", id, ErrUnknownSession)
	}
	if ch.cmd == nil || ch.cmd.Process == nil {
		return nil
	}
	if err := ch.cmd.Process.Signal(os.Interrupt); err != nil {
		return fmt.Errorf("serve: supervisor: interrupt %s: %w", id, err)
	}
	return nil
}

// Kill hard-stops the child and waits for the reap, so the lease is
// free by the time it returns. A session with no child is not an error.
func (s *Supervisor) Kill(id string) error {
	s.mu.Lock()
	ch, ok := s.kids[id]
	s.mu.Unlock()
	if !ok {
		return nil
	}
	s.killChild(ch)
	return nil
}

func (s *Supervisor) killChild(ch *child) {
	if ch.cmd != nil && ch.cmd.Process != nil {
		_ = ch.cmd.Process.Kill()
	}
	if ch.stdin != nil {
		ch.inMu.Lock()
		_ = ch.stdin.Close()
		ch.inMu.Unlock()
	}
	<-ch.done
}

func (s *Supervisor) Live(id string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.kids[id]
	return ok
}

// PendingAsk is the ask this session is blocked on, from the event
// stream rather than history: it is the live process that will read
// the next line.
func (s *Supervisor) PendingAsk(id string) *Ask {
	s.mu.Lock()
	defer s.mu.Unlock()
	a, ok := s.asks[id]
	if !ok || a == nil {
		return nil
	}
	cp := *a
	cp.Options = append([]string(nil), a.Options...)
	return &cp
}

// Recent is a copy of the session's event ring, oldest first.
func (s *Supervisor) Recent(id string) []Event {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Event(nil), s.events[id]...)
}

// Subscribe returns a buffered stream of the session's future events
// and an idempotent unsubscribe. The channel closes when the
// subscriber falls too far behind, or at Close.
func (s *Supervisor) Subscribe(id string) (<-chan Event, func()) {
	ch := make(chan Event, subBuffer)
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		close(ch)
		return ch, func() {}
	}
	if s.subs[id] == nil {
		s.subs[id] = map[int]chan Event{}
	}
	s.nextID++
	key := s.nextID
	s.subs[id][key] = ch
	s.mu.Unlock()

	var once sync.Once
	return ch, func() {
		once.Do(func() {
			s.mu.Lock()
			defer s.mu.Unlock()
			// The fan-out may have dropped and closed it already;
			// closing twice panics, so presence in the map decides.
			if cur, ok := s.subs[id][key]; ok {
				delete(s.subs[id], key)
				close(cur)
			}
		})
	}
}

func (s *Supervisor) Meta(id string) SessionMeta {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.meta[id]
}

func (s *Supervisor) AllMeta() map[string]SessionMeta {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make(map[string]SessionMeta, len(s.meta))
	for k, v := range s.meta {
		out[k] = v
	}
	return out
}

func (s *Supervisor) SetTitle(id, title string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Title = title
	s.meta[id] = m
	return s.saveMetaLocked()
}

// newProjectID is a time-ordered id, so a listing is stable and two
// projects made in the same second cannot collide.
func newProjectID() string { return history.NewID() }

// Projects lists the groupings, newest id last (ids are time-ordered),
// with the session count each one holds.
func (s *Supervisor) Projects() []Project {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Project, 0, len(s.projects))
	for _, p := range s.projects {
		out = append(out, p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// NewProject creates a grouping and returns it.
func (s *Supervisor) NewProject(name string) (Project, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return Project{}, fmt.Errorf("serve: supervisor: a project needs a name")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.projects == nil {
		s.projects = map[string]Project{}
	}
	p := Project{ID: newProjectID(), Name: name}
	s.projects[p.ID] = p
	return p, s.saveMetaLocked()
}

// RenameProject changes a grouping's name; membership is untouched.
func (s *Supervisor) RenameProject(id, name string) error {
	name = strings.TrimSpace(name)
	if name == "" {
		return fmt.Errorf("serve: supervisor: a project needs a name")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.projects[id]
	if !ok {
		return fmt.Errorf("serve: supervisor: no project %q", id)
	}
	p.Name = name
	s.projects[id] = p
	return s.saveMetaLocked()
}

// DeleteProject removes a grouping and unassigns its sessions. Deleting
// a project never deletes a conversation — the grouping is a label, and
// losing the label must not lose the work.
func (s *Supervisor) DeleteProject(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.projects[id]; !ok {
		return fmt.Errorf("serve: supervisor: no project %q", id)
	}
	delete(s.projects, id)
	for sid, m := range s.meta {
		if m.Project == id {
			m.Project = ""
			s.meta[sid] = m
		}
	}
	return s.saveMetaLocked()
}

// AssignProject puts a session in a grouping ("" removes it).
func (s *Supervisor) AssignProject(sessionID, projectID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if projectID != "" {
		if _, ok := s.projects[projectID]; !ok {
			return fmt.Errorf("serve: supervisor: no project %q", projectID)
		}
	}
	m := s.meta[sessionID]
	m.Project = projectID
	s.meta[sessionID] = m
	return s.saveMetaLocked()
}

// SetModel asks a session to switch model by writing the same /model
// command a person would type. The child owns the change — its llm row
// reconfigures live — so nothing restarts and there is no second code
// path to keep in step with the TUI.
func (s *Supervisor) SetModel(id, model string) error {
	model = strings.TrimSpace(model)
	if model == "" {
		return fmt.Errorf("serve: supervisor: model is required")
	}
	if err := s.Send(id, "/model "+model); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Model = model
	s.meta[id] = m
	return s.saveMetaLocked()
}

// SetEffort asks a session for more or less reasoning, via /think.
func (s *Supervisor) SetEffort(id, level string) error {
	level = strings.ToLower(strings.TrimSpace(level))
	if level == "" || !llm.ValidEffort(level) {
		return fmt.Errorf("serve: supervisor: %q is not a reasoning level (have %s)", level, strings.Join(llm.Efforts, ", "))
	}
	if err := s.Send(id, "/think "+level); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Effort = level
	s.meta[id] = m
	return s.saveMetaLocked()
}

// SetArchived hides a session. Archiving kills its child first: an
// archived session that kept writing history would be a ghost writer.
// It is reversible, and unarchiving does not respawn anything.
func (s *Supervisor) SetArchived(id string, archived bool) error {
	if archived {
		if err := s.Kill(id); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Archived = archived
	s.meta[id] = m
	return s.saveMetaLocked()
}

func (s *Supervisor) loadMeta() error {
	b, err := os.ReadFile(s.opt.MetaPath)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("serve: supervisor: read %s: %w", s.opt.MetaPath, err)
	}
	// The file began as a bare map of session id -> meta and grew a
	// projects table. Read both shapes: an older file must not lose its
	// titles and archive flags just because the format moved on.
	var f metaFile
	if err := json.Unmarshal(b, &f); err == nil && (f.Sessions != nil || f.Projects != nil) {
		for k, v := range f.Sessions {
			s.meta[k] = v
		}
		for k, v := range f.Projects {
			s.projects[k] = v
		}
		return nil
	}
	var m map[string]SessionMeta
	if err := json.Unmarshal(b, &m); err != nil {
		return fmt.Errorf("serve: supervisor: parse %s: %w", s.opt.MetaPath, err)
	}
	for k, v := range m {
		s.meta[k] = v
	}
	return nil
}

// metaFile is what meta.json holds now: sessions and the groupings they
// belong to, in one atomically-written file.
type metaFile struct {
	Sessions map[string]SessionMeta `json:"sessions,omitempty"`
	Projects map[string]Project     `json:"projects,omitempty"`
}

// saveMetaLocked rewrites meta.json atomically; caller holds s.mu.
func (s *Supervisor) saveMetaLocked() error {
	if s.opt.MetaPath == "" {
		return nil
	}
	b, err := json.MarshalIndent(metaFile{Sessions: s.meta, Projects: s.projects}, "", "  ")
	if err != nil {
		return fmt.Errorf("serve: supervisor: encode meta: %w", err)
	}
	tmp := s.opt.MetaPath + ".tmp"
	if err := os.WriteFile(tmp, append(b, '\n'), 0o644); err != nil {
		return fmt.Errorf("serve: supervisor: write %s: %w", tmp, err)
	}
	if err := os.Rename(tmp, s.opt.MetaPath); err != nil {
		os.Remove(tmp)
		return fmt.Errorf("serve: supervisor: replace %s: %w", s.opt.MetaPath, err)
	}
	return nil
}

// Close kills and reaps every child and closes every subscriber. After
// it returns, no lease is held and no goroutine of this supervisor is
// still writing.
func (s *Supervisor) Close() error {
	s.mu.Lock()
	kids := make([]*child, 0, len(s.kids))
	for _, ch := range s.kids {
		kids = append(kids, ch)
	}
	s.mu.Unlock()

	for _, ch := range kids {
		s.killChild(ch)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
	for id, subs := range s.subs {
		for key, cch := range subs {
			close(cch)
			delete(subs, key)
		}
		delete(s.subs, id)
	}
	return nil
}
