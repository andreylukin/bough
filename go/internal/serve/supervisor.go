// Supervisor owns bough sessions as child processes: one
// `bough --headless --json` per session, never two. history's
// ConcurrentWriter exists because two processes appending one session
// file is a real hazard, so the lease map here is the whole point of
// the package — everything else (the event ring, the fan-out, the
// archive/title store) hangs off it.
package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
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
	// Project is the project this session belongs to, by SLUG — the
	// directory name under ~/.bough/projects. "" is ungrouped, which is
	// the normal state; a session is never forced into one.
	Project string `json:"project,omitempty"`
	// Ack is the last history seq a person marked seen. A failure or an
	// unexpected interruption recorded after it still needs them; one at
	// or before it has been dealt with. A later failure resurfaces.
	Ack int64 `json:"ack,omitempty"`
	// SpawnedBy is the parent of a background agent, persisted so the
	// tree survives a serve restart for children that ran.
	SpawnedBy string `json:"spawnedBy,omitempty"`
	// Thread marks a session a person started from the project page. It
	// is parented to main only so its finish lands there; it is a
	// top-level agent and may start agents of its own, where a child an
	// agent started may not (depth 1).
	Thread bool `json:"thread,omitempty"`
	// Queued is derived from Task on load.
	Queued bool `json:"-"`
	// Task is a queued child's pending start, persisted so the queue
	// survives a serve restart instead of leaving a ghost that never
	// runs yet counts against its parent's budget. Cleared on launch.
	Task *ChildTask `json:"task,omitempty"`
	// Reported is the seq of the child's last turn reported to its
	// parent. On disk because a restarted serve that re-adopts a child
	// would otherwise report its old turn again when the process exits.
	Reported int64 `json:"reported,omitempty"`
}

// Project is one project. The DIRECTORY is the project:
// ~/.bough/projects/<slug> holds project.yml, the build scripts and
// MEMORY.md, and everything here is derived from it on each read — so
// the agent can create, edit and delete projects with the file tools and
// serve never disagrees with the disk. There is no table.
type Project struct {
	// Slug is the directory name and the key: sessions, orbs, images and
	// caches all name it, so it never changes. Renaming sets Name.
	Slug string `json:"slug"`
	// Name is project.yml's `name:`, falling back to the slug.
	Name string `json:"name"`
	// Error is why project.yml did not parse, "" when it did. A broken
	// definition is still a project: the editor that fixes it lives on
	// the project's own page, and dropping it would drop the editor too.
	Error string `json:"error,omitempty"`
}

// projectOf turns a directory listing entry into the wire shape.
func projectOf(e projectdef.Entry) Project {
	p := Project{Slug: e.Slug, Name: projectdef.Project{Slug: e.Slug, Def: e.Def}.DisplayName()}
	if e.Err != nil {
		p.Error = e.Err.Error()
	}
	return p
}

// CreateOptions is what a new session starts as. Mode "" is local.
type CreateOptions struct {
	Cwd, Prompt, Mode, Slug string
	SpawnedBy               string   // "" = a person's session
	ID                      string   // pre-minted id -> BOUGH_SESSION_ID, no dir-diff discovery
	Args                    []string // extra argv after --headless --json (e.g. --set llm.model=x)
	Env                     []string // extra env
	Origin                  string   // BOUGH_ORIGIN override; "" = web
	Model                   string   // background agent: "plugin/model" to start on; "" = config
	Thread                  bool     // a person's project thread: SpawnedBy is main for notices only
}

// Options configures a Supervisor. Every path is explicit so tests can
// run against a t.TempDir() HOME.
type Options struct {
	Exe      string   // bough binary; "" => os.Executable()
	HistDir  string   // $HOME/.bough/history
	MetaPath string   // $HOME/.bough/serve/meta.json
	Env      []string // child env; nil => os.Environ()
	Buffer   int      // per-session event ring; <=0 => 500
	// Runtime answers image/container questions for orbs; nil =>
	// container.Default(). A field so tests inject container.Fake.
	Runtime container.Runtime
	// Home holds .bough/projects and .bough/orbs; "" => HistDir's
	// grandparent, which is HOME for the standard layout.
	Home string
}

var (
	ErrNoAsk          = errors.New("serve: supervisor: no pending ask")
	ErrBadAnswer      = errors.New("serve: supervisor: a secret answer cannot contain a newline")
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
	// cmd and stdin are set once, before ready closes, and never again:
	// a child is registered in kids before its process exists, so every
	// reader waits on ready (or done, for a start that failed) first.
	cmd   *exec.Cmd
	stdin io.WriteCloser
	ready chan struct{}
	inMu  sync.Mutex // one writer at a time: a torn line would be read as two prompts

	done chan struct{} // closed once the process is reaped and the lease dropped

	// Guarded by Supervisor.mu.
	// booting: a background agent reserved for launch that has not yet
	// been handed its task. stopReq: stopped while booting, so launch
	// kills it instead of prompting.
	booting, stopReq bool
	id               string
	metaID           string // a session id the child volunteered on a "meta" line
	buffer           []pending
	dropped          bool // the lease has already been handed back
	// unread: a prompt was written that the child has not yet reported
	// taking (no "input"/"steer" since). held: an interrupt that came in
	// that gap, sent once the child takes the prompt. A SIGINT before
	// then cancels nothing: a booting child dies of it, a booted one
	// exits without the turn ever starting.
	unread, held bool
	holdGen      int // which hold a hold-limit timer belongs to
	// inTurn: the child took an input and has not ended that turn. A
	// line sent then is a steer that lands only at the next boundary,
	// so an interrupt must go straight through, not wait for it.
	inTurn bool
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
	home string
	rt   container.Runtime
	// started is when this serve began: a state.json PID written before
	// it, by a process that is not our child, may be a reused pid.
	started time.Time

	// createMu serializes Create so two callers cannot both claim the
	// same freshly-appeared history id.
	createMu sync.Mutex

	// mainMu guards mainLocks; each entry serializes Main for ONE slug.
	// Not s.mu: minting a main thread runs Create, which blocks for up
	// to createTimeout, and holding the supervisor's mutex across that
	// would stall every other session.
	mainMu    sync.Mutex
	mainLocks map[string]*sync.Mutex

	mu     sync.Mutex
	kids   map[string]*child
	events map[string][]Event
	seq    map[string]int64
	asks   map[string]*Ask
	subs   map[string]map[int]chan Event
	deltas map[string]*deltaState
	nextID int
	meta   map[string]SessionMeta
	closed bool
	// metaVersion and legacy are meta.json as it was READ: the schema
	// version, and the pre-version-2 label table migrateProjects folds
	// into ~/.bough/projects. Both are empty once the migration ran.
	metaVersion int
	legacy      map[string]oldProject
	// mains is slug -> the project's main thread. Serve state, not part
	// of the definition: a project directory copied to another machine
	// must not claim a session id that machine never had.
	mains map[string]string
	// stoppedAt is when serve stopped a session's container. The child
	// is state.json's only writer, so until it rewrites the file a
	// stale "running" is read as stopped.
	stoppedAt map[string]time.Time
	// building is the slugs with an image build this serve started.
	building map[string]bool
	// buildErr is the last failed build per slug this serve started,
	// kept so a failure build.json never recorded still shows.
	buildErr map[string]string

	// Background agents (children.go): the FIFO of children waiting for
	// a slot, the ones holding a slot, and the global cap last requested.
	queue      []queuedChild
	running    map[string]bool
	maxRunning int

	// spawnArgs is the extra argv and env a Create asked for, per id, so
	// a respawn through ensure starts the session the same way.
	spawnArgs map[string]spawnSpec
}

type spawnSpec struct{ args, env []string }

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
	home := opt.Home
	if home == "" {
		home = filepath.Dir(filepath.Dir(opt.HistDir))
	}
	rt := opt.Runtime
	if rt == nil {
		rt = container.Default()
	}
	s := &Supervisor{
		home:      home,
		rt:        rt,
		started:   time.Now(),
		stoppedAt: map[string]time.Time{},
		building:  map[string]bool{},
		buildErr:  map[string]string{},
		opt:       opt,
		exe:       exe,
		cwd:       cwd,
		ring:      ring,
		kids:      map[string]*child{},
		events:    map[string][]Event{},
		seq:       map[string]int64{},
		asks:      map[string]*Ask{},
		subs:      map[string]map[int]chan Event{},
		deltas:    map[string]*deltaState{},
		meta:      map[string]SessionMeta{},
		mains:     map[string]string{},
		running:   map[string]bool{},
	}
	if opt.MetaPath != "" {
		if err := os.MkdirAll(filepath.Dir(opt.MetaPath), 0o755); err != nil {
			return nil, fmt.Errorf("serve: supervisor: meta dir: %w", err)
		}
		if err := s.loadMeta(); err != nil {
			return nil, err
		}
	}
	// After loadMeta, never inside it: loadMeta is a pure read, and this
	// writes both ~/.bough/projects and meta.json.
	if err := s.migrateProjects(); err != nil {
		return nil, err
	}
	// Children queued before a restart start now, as they would have.
	if len(s.queue) > 0 {
		go s.drainQueue()
	}
	return s, nil
}

func (s *Supervisor) HistDir() string { return s.opt.HistDir }

// Home is the directory holding .bough/projects and .bough/orbs.
func (s *Supervisor) Home() string { return s.home }

// Runtime is the container engine orbs are asked about.
func (s *Supervisor) Runtime() container.Runtime { return s.rt }

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
func (s *Supervisor) Create(opt CreateOptions) (string, error) {
	cwd, prompt := opt.Cwd, opt.Prompt
	var extra []string
	switch opt.Mode {
	case "", "local":
	case "project":
		if err := projectdef.ValidSlug(opt.Slug); err != nil {
			return "", fmt.Errorf("serve: supervisor: project session: %w", err)
		}
		// The child builds its own orb and chdirs into the worktree; it
		// starts from home so nothing ties it to serve's cwd.
		cwd = s.home
		extra = []string{"BOUGH_MODE=project", "BOUGH_PROJECT=" + opt.Slug}
	default:
		return "", fmt.Errorf("serve: supervisor: unknown session mode %q", opt.Mode)
	}
	extra = append(extra, opt.Env...)
	if opt.Origin != "" {
		extra = append(extra, "BOUGH_ORIGIN="+opt.Origin) // after web's: the last value wins
	}
	if opt.ID != "" {
		return s.createWithID(opt.ID, cwd, prompt, extra, opt.Args)
	}
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

	ch, err := s.spawn(cwd, "", extra, nil)
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
				if err := s.writePrompt(ch, prompt); err != nil {
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

// createWithID starts a session under an id the caller minted, the
// way CreateChild does: the child names its history file after
// BOUGH_SESSION_ID, so discovery is waiting for that one file.
func (s *Supervisor) createWithID(id, cwd, prompt string, extra, args []string) (string, error) {
	extra = append(slices.Clone(extra), "BOUGH_SESSION_ID="+id)
	s.mu.Lock()
	if s.spawnArgs == nil {
		s.spawnArgs = map[string]spawnSpec{}
	}
	// Mode and BOUGH_SESSION_ID are not kept: a resumed child reads
	// its mode from its own file and is started with -r.
	var env []string
	for _, kv := range extra {
		if strings.HasPrefix(kv, "BOUGH_ORIGIN=") {
			env = append(env, kv)
		}
	}
	s.spawnArgs[id] = spawnSpec{args: slices.Clone(args), env: env}
	s.mu.Unlock()

	ch, err := s.spawn(cwd, "", extra, args)
	if err != nil {
		return "", err
	}
	path := filepath.Join(s.opt.HistDir, id+".jsonl")
	deadline := time.Now().Add(createTimeout)
	for {
		if _, err := os.Stat(path); err == nil {
			if err := s.claim(ch, id); err != nil {
				s.killChild(ch)
				return "", err
			}
			if prompt != "" {
				if err := s.writePrompt(ch, prompt); err != nil {
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
			return "", fmt.Errorf("serve: supervisor: %s did not appear in %s", path, createTimeout)
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
	ch := newChild(id)
	s.kids[id] = ch
	spec := s.spawnArgs[id]
	s.mu.Unlock()

	if err := s.start(ch, s.adoptDir(id), id, spec.env, spec.args); err != nil {
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
func (s *Supervisor) spawn(cwd, id string, extra, args []string) (*child, error) {
	ch := newChild("")
	if err := s.start(ch, cwd, id, extra, args); err != nil {
		close(ch.done)
		return nil, err
	}
	return ch, nil
}

// start builds and launches the process and the goroutines that read
// it. NEVER a bare --resume: headless refuses it, prints the session
// list and exits 2.
func (s *Supervisor) start(ch *child, dir, id string, extra, more []string) error {
	if ch.ready != nil {
		defer close(ch.ready)
	}
	args := []string{"--headless", "--json"}
	args = append(args, more...)
	if id != "" {
		args = append(args, "-r", id)
	}
	cmd := exec.Command(s.exe, args...)
	cmd.Dir = dir
	cmd.Env = s.opt.Env
	if cmd.Env == nil {
		cmd.Env = os.Environ()
	}
	// Every child serve runs is the person's: created from the page, or
	// resumed because they sent it something.
	// A serve that itself inherited a mode must not hand it to a local
	// child, and a resumed child takes its mode from its own file: strip
	// both, then add back only what this spawn asked for.
	cmd.Env = slices.DeleteFunc(slices.Clone(cmd.Env), func(kv string) bool {
		return strings.HasPrefix(kv, "BOUGH_MODE=") || strings.HasPrefix(kv, "BOUGH_PROJECT=") ||
			strings.HasPrefix(kv, "BOUGH_PROJECT_DIR=")
	})
	cmd.Env = append(cmd.Env, "BOUGH_ORIGIN=web")
	cmd.Env = append(cmd.Env, extra...)
	// Derived at EVERY start, never baked into spawnArgs: the membership
	// lives in meta.json, is usually set long after the session was
	// created, and spawnArgs is in-memory and empty after a serve
	// restart. A session already running does not pick it up — the
	// injection starts at its next start.
	cmd.Env = append(cmd.Env, s.projectEnv(id)...)
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

// projectEnv is the project env a session's child process needs beyond
// what its own history file says. A project session gets its mode and
// slug from its file; this is the LOCAL session assigned to a project,
// which gets the project directory so context-md prepends its MEMORY.md
// and tools may write it. Nothing for a session with no project, and
// nothing for a project whose directory has since been deleted.
func (s *Supervisor) projectEnv(id string) []string {
	if id == "" {
		return nil // a Create: the session has no meta entry yet
	}
	slug := s.Meta(id).Project
	if slug == "" || projectdef.ValidSlug(slug) != nil {
		return nil
	}
	dir := filepath.Join(projectdef.Root(s.home), slug)
	if st, err := os.Stat(dir); err != nil || !st.IsDir() {
		return nil
	}
	env := []string{"BOUGH_PROJECT_DIR=" + dir}
	// Which side of the project the session is on. Derived here for the
	// same reason as the directory: the main thread is recorded in
	// meta.json, not in the spawn arguments, so a restarted serve still
	// tells it what it is.
	if s.isMain(id) {
		env = append(env, "BOUGH_PROJECT_MAIN=1")
	}
	if s.Meta(id).Thread {
		env = append(env, "BOUGH_PROJECT_THREAD=1")
	}
	return env
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
	switch kind {
	case "input", "steer":
		ch.unread = false
		s.releaseLocked(ch)
	case "assistant-delta", "thinking-delta", "assistant", "thinking", "code", "result", "call", "call-delta":
		// The real headless child never prints "input": its first
		// output is the sign the prompt became a turn. On the engine a
		// turn can open on a native call with no code block around it,
		// and a Stop would otherwise be held while ch.unread is set.
		if ch.unread {
			ch.unread = false
			s.releaseLocked(ch)
		}
		ch.inTurn = true
	}
	switch kind {
	case "input":
		ch.inTurn = true
	case "done", "cancelled", "exit":
		ch.inTurn = false
	}
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
	if isDelta(kind) {
		s.bufferDeltaLocked(id, kind, text, extra)
		return
	}
	if kind == "delta-reset" {
		// The engine's reset of streamed text a retry or a newer request
		// replaced. Live only, like the fragments it clears: it never
		// enters the ring, or a late joiner would replay a reset of text
		// it never saw.
		s.dropTextDeltasLocked(id)
		s.flushDeltasLocked(id)
		s.fanoutLocked(id, Event{Session: id, Seq: 0, At: time.Now(), Kind: kind, Text: text, Extra: extra})
		return
	}
	// Anything recorded supersedes the fragments that led to it, so
	// drain them first and keep the browser's ordering honest.
	s.flushDeltasLocked(id)

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
	s.childEventLocked(id, kind, extra)

	s.fanoutLocked(id, ev)
}

// fanoutLocked delivers one event to every live subscriber of a
// session. Caller holds s.mu.
func (s *Supervisor) fanoutLocked(id string, ev Event) {
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
	a.Secret, _ = ev.Extra["secret"].(bool)
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
	return s.writePrompt(ch, text)
}

// Answer replies to the armed tools.ask over the same pipe.
func (s *Supervisor) Answer(id, text string) error {
	p := s.PendingAsk(id)
	if p == nil {
		return ErrNoAsk
	}
	// A secret rides as one raw stdin line: a newline would split it
	// (or wrap it as a prompt). Nothing here logs the text.
	if p.Secret && strings.ContainsAny(text, "\r\n") {
		return ErrBadAnswer
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

// holdLimit bounds how long an interrupt waits for the child to take
// its prompt, so a line that never becomes a turn cannot swallow it.
var holdLimit = 20 * time.Second

// writePrompt writes a line the child will run (or steer with), noting
// it unread first so an interrupt in the gap is held, not lost.
func (s *Supervisor) writePrompt(ch *child, text string) error {
	if !strings.HasPrefix(text, "/") && !strings.HasPrefix(text, "!") {
		s.mu.Lock()
		ch.unread = true
		s.mu.Unlock()
	}
	return s.write(ch, text)
}

func (s *Supervisor) write(ch *child, text string) error {
	ch.started()
	ch.inMu.Lock()
	defer ch.inMu.Unlock()
	if ch.stdin == nil {
		return fmt.Errorf("serve: supervisor: session has no stdin")
	}
	// The child reads a line per prompt; a multi-line one (a paste, a
	// shift+return) rides as the {"prompt": ...} line it also accepts,
	// or each of its lines would arrive as a prompt of its own.
	if strings.Contains(text, "\n") {
		b, _ := json.Marshal(map[string]string{"prompt": text})
		text = string(b)
	}
	if _, err := io.WriteString(ch.stdin, text+"\n"); err != nil {
		return fmt.Errorf("serve: supervisor: write stdin: %w", err)
	}
	return nil
}

func newChild(id string) *child {
	return &child{id: id, ready: make(chan struct{}), done: make(chan struct{})}
}

// started waits until start has set cmd and stdin (or given up), so
// they can be read without a lock. False: the child never started.
func (ch *child) started() bool {
	if ch.ready == nil {
		return true // a test's hand-built child
	}
	select {
	case <-ch.ready:
		return ch.cmd != nil
	case <-ch.done:
		return false
	}
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
	if !ch.started() || ch.cmd == nil || ch.cmd.Process == nil {
		return nil
	}
	s.mu.Lock()
	if ch.unread && !ch.inTurn {
		if !ch.held {
			ch.held = true
			ch.holdGen++
		}
		gen := ch.holdGen
		s.mu.Unlock()
		time.AfterFunc(holdLimit, func() {
			s.mu.Lock()
			defer s.mu.Unlock()
			// A stale timer from an earlier, already-released hold must
			// not fire this one before its prompt is taken.
			if ch.holdGen == gen {
				s.releaseLocked(ch)
			}
		})
		return nil
	}
	s.mu.Unlock()
	if err := ch.cmd.Process.Signal(os.Interrupt); err != nil {
		return fmt.Errorf("serve: supervisor: interrupt %s: %w", id, err)
	}
	return nil
}

// releaseLocked sends a held interrupt. Caller holds s.mu.
func (s *Supervisor) releaseLocked(ch *child) {
	if !ch.held || ch.dropped {
		return
	}
	ch.held = false
	_ = ch.cmd.Process.Signal(os.Interrupt)
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
	s.stopKilledOrb(id)
	return nil
}

// stopKilledOrb stops a killed project child's container. The child stops
// its own orb when its row unmounts, but SIGKILL skips that, so every
// killed project session (a loop node, a coach, an archived agent) left a
// container running. Stop, never Remove: a resume reuses it.
func (s *Supervisor) stopKilledOrb(id string) {
	if s.rt == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	name := container.OrbName(id)
	if st, err := s.rt.Inspect(ctx, name); err == nil && st == container.StateRunning {
		if err := s.stopOrb(ctx, id); err != nil {
			fmt.Fprintf(os.Stderr, "bough: serve: stop orb %s: %v\n", name, err)
		}
	}
}

// stopOrb stops a session's container on serve's side. state.json is
// marked stopped first, so the child can tell a job this kills from a job
// that failed (and not wake a turn for it); a failed stop puts it back.
func (s *Supervisor) stopOrb(ctx context.Context, id string) error {
	if err := orb.StopContainer(ctx, s.rt, s.Home(), id); err != nil {
		return err
	}
	s.mu.Lock()
	s.stoppedAt[id] = time.Now()
	s.mu.Unlock()
	return nil
}

func (s *Supervisor) killChild(ch *child) {
	if !ch.started() {
		<-ch.done
		return
	}
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

// childPID is the pid of the session's live child, 0 when none.
func (s *Supervisor) childPID(id string) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	ch, ok := s.kids[id]
	if !ok {
		return 0
	}
	select {
	case <-ch.ready:
	default:
		if ch.ready != nil {
			return 0 // still starting; not waited for under s.mu
		}
	}
	if ch.cmd == nil || ch.cmd.Process == nil {
		return 0
	}
	return ch.cmd.Process.Pid
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

// Projects lists every project directory, by display name. It reads the
// filesystem on each call: a project the agent created with the file
// tools shows up without serve being told, and one it deleted stops
// showing up. A broken project.yml is listed with its parse error.
func (s *Supervisor) Projects() []Project {
	ents := projectdef.ListAll(s.home)
	out := make([]Project, 0, len(ents))
	for _, e := range ents {
		out = append(out, projectOf(e))
	}
	// Name, then slug: two projects can carry the same name, and ListAll
	// already ordered by slug, so the tiebreak keeps the listing stable.
	sort.SliceStable(out, func(i, j int) bool {
		if out[i].Name != out[j].Name {
			return out[i].Name < out[j].Name
		}
		return out[i].Slug < out[j].Slug
	})
	return out
}

// Project returns one project by slug.
func (s *Supervisor) Project(slug string) (Project, bool) {
	if projectdef.ValidSlug(slug) != nil {
		return Project{}, false
	}
	b, err := os.ReadFile(filepath.Join(projectdef.Root(s.home), slug, projectdef.FileYAML))
	if err != nil {
		return Project{}, false
	}
	e := projectdef.Entry{Slug: slug, Dir: filepath.Join(projectdef.Root(s.home), slug)}
	if e.Def, e.Err = projectdef.Parse(b); e.Err != nil {
		e.Err = fmt.Errorf("projectdef: load %s: %w", slug, e.Err)
	}
	return projectOf(e), true
}

// ErrProjectExists is a slug already on disk. The filesystem is the
// uniqueness check — there is no table to disagree with it.
var ErrProjectExists = errors.New("serve: supervisor: a project with that name already exists")

// ErrBadName is a name nothing can be made of: blank, or with no letter
// or digit to name a directory after.
var ErrBadName = errors.New("serve: supervisor: a project needs a name")

// NewProject creates ~/.bough/projects/<slug> from a name and returns
// it. The definition has no repos: which repos a project works on is
// chosen on its page, not guessed from its name.
func (s *Supervisor) NewProject(name string) (Project, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return Project{}, ErrBadName
	}
	slug := slugify(name)
	if err := projectdef.ValidSlug(slug); err != nil {
		return Project{}, fmt.Errorf("%w: %q has no letter or digit to name a directory after", ErrBadName, name)
	}
	if _, err := projectdef.CreateEmpty(s.home, slug, name); err != nil {
		if errors.Is(err, fs.ErrExist) {
			return Project{}, fmt.Errorf("%w: ~/.bough/projects/%s", ErrProjectExists, slug)
		}
		return Project{}, fmt.Errorf("serve: supervisor: create project: %w", err)
	}
	return Project{Slug: slug, Name: name}, nil
}

// RenameProject changes the display name in project.yml. The directory
// keeps its slug: orbs, images, caches and every past session name it.
func (s *Supervisor) RenameProject(slug, name string) error {
	name = strings.TrimSpace(name)
	if name == "" {
		return ErrBadName
	}
	p, ok := s.Project(slug)
	if !ok {
		return fmt.Errorf("serve: supervisor: no project %q: %w", slug, ErrUnknownProject)
	}
	if err := projectdef.SetName(s.home, slug, name); err != nil {
		// SetName splices only the name: line, so a definition broken
		// anywhere else is still broken after it. That is the file's
		// fault, fixed in the editor, not a server error: it came back
		// as a 500 the page could only show as "something went wrong".
		if p.Error != "" {
			return fmt.Errorf("serve: supervisor: rename %s: %w: %w", slug, ErrBrokenProject, err)
		}
		return fmt.Errorf("serve: supervisor: rename %s: %w", slug, err)
	}
	return nil
}

// ErrBrokenProject is a change refused because project.yml does not
// parse.
var ErrBrokenProject = errors.New("serve: supervisor: project.yml does not parse; fix it in the editor first")

// DeleteProject removes the project directory and the state a project of
// the same name would otherwise inherit — its images, its repo cache and
// its container caches — and unassigns its sessions.
//
// It never deletes a conversation or its history. It DOES delete
// hand-written files that were never committed anywhere (Dockerfile,
// setup.sh, resume.sh, MEMORY.md), which is why the web asks for the
// slug to be typed before calling it. Archive keeps everything.
func (s *Supervisor) DeleteProject(slug string) error {
	if _, ok := s.Project(slug); !ok {
		return fmt.Errorf("serve: supervisor: no project %q: %w", slug, ErrUnknownProject)
	}
	// Before the files: a thread still running would rebuild its orb
	// from a definition that is about to stop existing.
	if err := s.EndProject(slug); err != nil {
		return err
	}
	for _, dir := range []string{
		filepath.Join(projectdef.Root(s.home), slug),
		filepath.Join(s.home, ".bough", "orbs", "images", slug),
		filepath.Join(s.home, ".bough", "orbs", "cache", slug),
		filepath.Join(s.home, ".bough", "cache", slug),
	} {
		if err := os.RemoveAll(dir); err != nil {
			return fmt.Errorf("serve: supervisor: delete project %s: %w", slug, err)
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.mains, slug)
	for sid, m := range s.meta {
		if m.Project == slug {
			m.Project = ""
			s.meta[sid] = m
		}
	}
	return s.saveMetaLocked()
}

// ErrUnknownProject is a slug with no directory.
var ErrUnknownProject = errors.New("serve: supervisor: unknown project")

// ErrProjectSession is a project session someone tried to file
// elsewhere: its project is where its orb, its worktrees and its
// MEMORY.md come from, recorded in its history and not a label.
var ErrProjectSession = errors.New("serve: supervisor: a project session lives in its project's orb; start a thread in the other project instead")

// AssignProject files a session under a project ("" takes it out of
// one). Local sessions only: see ErrProjectSession.
func (s *Supervisor) AssignProject(sessionID, slug string) error {
	if slug != "" {
		if _, ok := s.Project(slug); !ok {
			return fmt.Errorf("serve: supervisor: no project %q: %w", slug, ErrUnknownProject)
		}
	}
	entries, _ := s.Entries(sessionID)
	if mode, _ := sessionMode(entries); mode == "project" {
		return ErrProjectSession
	}
	return s.setProject(sessionID, slug)
}

// setProject writes the membership with no mode check: for a session
// serve itself just started in a project's orb, where the history meta
// entry AssignProject would read may not be on disk yet.
func (s *Supervisor) setProject(sessionID, slug string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[sessionID]
	m.Project = slug
	s.meta[sessionID] = m
	return s.saveMetaLocked()
}

// Main is the project's main thread, created on first use. It is one
// long-lived session in the project's orb and the parent of every other
// session in the project, so the finish and failure notices a thread's
// last turn produces land in a conversation a person is already reading.
//
// Creation is serialized per slug, and the id is persisted BEFORE the
// child is spawned: two browser tabs opening the same project at the
// same moment would otherwise each mint a main and the project would
// have two. A recorded main whose history file is gone is re-minted.
func (s *Supervisor) Main(slug string) (string, error) {
	if _, ok := s.Project(slug); !ok {
		return "", fmt.Errorf("serve: supervisor: no project %q: %w", slug, ErrUnknownProject)
	}
	lock := s.mainLock(slug)
	lock.Lock()
	defer lock.Unlock()

	s.mu.Lock()
	id := s.mains[slug]
	if id != "" && s.historyExists(id) {
		s.mu.Unlock()
		return id, nil
	}
	id = history.NewID()
	s.mains[slug] = id
	// The membership too: until the child writes its own meta entry,
	// this is the only thing that says the session is in the project.
	// SpawnedBy stays "" — a main with a parent could not spawn.
	m := s.meta[id]
	m.Project = slug
	s.meta[id] = m
	err := s.saveMetaLocked()
	s.mu.Unlock()
	if err != nil {
		return "", err
	}
	// Outside s.mu: Create takes createMu and waits for the child's
	// history file, which is seconds, not microseconds.
	if _, err := s.Create(CreateOptions{Mode: "project", Slug: slug, ID: id}); err != nil {
		return "", fmt.Errorf("serve: supervisor: project %s: start the main thread: %w", slug, err)
	}
	return id, nil
}

// MainID is the project's main thread WITHOUT creating one: "" when it
// has none yet, or when the recorded one's history file is gone.
func (s *Supervisor) MainID(slug string) string {
	s.mu.Lock()
	id := s.mains[slug]
	s.mu.Unlock()
	if id == "" || !s.historyExists(id) {
		return ""
	}
	return id
}

// isMain says whether a session is some project's main thread.
func (s *Supervisor) isMain(id string) bool {
	if id == "" {
		return false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, main := range s.mains {
		if main == id {
			return true
		}
	}
	return false
}

func (s *Supervisor) mainLock(slug string) *sync.Mutex {
	s.mainMu.Lock()
	defer s.mainMu.Unlock()
	if s.mainLocks == nil {
		s.mainLocks = map[string]*sync.Mutex{}
	}
	if lock, ok := s.mainLocks[slug]; ok {
		return lock
	}
	lock := &sync.Mutex{}
	s.mainLocks[slug] = lock
	return lock
}

func (s *Supervisor) historyExists(id string) bool {
	_, err := os.Stat(filepath.Join(s.opt.HistDir, id+".jsonl"))
	return err == nil
}

// projectSessions is every session of a project except its main thread:
// the threads main started, and the ones filed under the project that it
// did not (started from the CLI, or from before there was a main).
func (s *Supervisor) projectSessions(slug, main string) []string {
	seen := map[string]bool{main: true}
	var out []string
	add := func(id string) {
		if id == "" || seen[id] {
			return
		}
		seen[id] = true
		out = append(out, id)
	}
	if main != "" {
		for _, c := range s.Children(main) {
			add(c.ID)
		}
	}
	s.mu.Lock()
	var filed []string
	for id, m := range s.meta {
		if m.Project == slug {
			filed = append(filed, id)
		}
	}
	s.mu.Unlock()
	for _, id := range filed {
		add(id)
	}
	sort.Strings(out)
	return out
}

// EndProject stops everything the project has running: every thread,
// then the main thread itself.
//
// Orbs are per SESSION (container.OrbName is "bough-orb-"+session), so a
// project with a main and N threads runs N+1 containers. EndChild stops
// a CHILD's; main is nobody's child, so its container is stopped here.
func (s *Supervisor) EndProject(slug string) error {
	main := s.MainID(slug)
	var errs []error
	for _, id := range s.projectSessions(slug, main) {
		if err := s.EndChild(id); err != nil {
			errs = append(errs, err)
		}
	}
	if main == "" {
		return errors.Join(errs...)
	}
	if err := s.Kill(main); err != nil {
		errs = append(errs, err)
	}
	// Kill stops the orb of a child it was holding; a main nothing holds
	// (serve restarted, the session idle) leaves Kill a no-op, and its
	// container would keep running.
	if s.rt != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		if st, err := s.rt.Inspect(ctx, container.OrbName(main)); err == nil && st == container.StateRunning {
			if err := s.stopOrb(ctx, main); err != nil {
				errs = append(errs, fmt.Errorf("serve: supervisor: stop orb of %s: %w", main, err))
			}
		}
	}
	return errors.Join(errs...)
}

// SetModel asks a session to switch model by writing the same /model
// command a person would type. The child owns the change — its llm row
// reconfigures live — so nothing restarts and there is no second code
// path to keep in step with the TUI. plugin names the provider that
// owns the model: a bare id keeps whichever provider the row runs, so
// another provider's model would still go to the old one.
func (s *Supervisor) SetModel(id, plugin, model string) error {
	model, plugin = strings.TrimSpace(model), strings.TrimSpace(plugin)
	if model == "" {
		return fmt.Errorf("serve: supervisor: model is required")
	}
	cmd := "/model " + model
	if plugin != "" {
		cmd = "/model " + plugin + " " + model
	}
	if err := s.Send(id, cmd); err != nil {
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
		return fmt.Errorf("serve: supervisor: %q is not a reasoning level (have %s)", level, strings.Join(llm.Levels(), ", "))
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

// Acknowledge marks everything the session has recorded so far as seen.
func (s *Supervisor) Acknowledge(id string) error {
	entries, err := s.Entries(id)
	if err != nil {
		return err
	}
	var last int64
	if n := len(entries); n > 0 {
		last = entries[n-1].Seq
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Ack = last
	s.meta[id] = m
	return s.saveMetaLocked()
}

// migrateProjects folds the version-1 label table into
// ~/.bough/projects, which is where a project lives now. It runs once,
// after loadMeta and before anything serves: the version stamp makes the
// next boot skip it in O(1). One bad project is logged and skipped —
// booting serve must not depend on every definition being writable.
func (s *Supervisor) migrateProjects() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.metaVersion >= metaVersion {
		return nil
	}
	existed := false
	if s.opt.MetaPath != "" {
		if _, err := os.Stat(s.opt.MetaPath); err == nil {
			existed = true
		}
	}
	// Deterministic order: the project-<n> numbering and the -2 suffixes
	// must not depend on map iteration.
	ids := make([]string, 0, len(s.legacy))
	for id := range s.legacy {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	// Slugs a labelled project already carries, and the directories
	// already on disk. Both, because a definition made outside this
	// migration — `bough project create`, or an agent writing
	// ~/.bough/projects/<slug>, after the last boot of the old binary —
	// belongs to no label: a label named "Bough" that adopted the real
	// bough project would rename it and file its sessions into someone
	// else's repos, image and orb.
	taken := map[string]bool{}
	for _, p := range s.legacy {
		if p.Slug != "" {
			taken[p.Slug] = true
		}
	}
	onDisk := map[string]projectdef.Entry{}
	for _, e := range projectdef.ListAll(s.home) {
		onDisk[e.Slug] = e
	}

	slugOf := make(map[string]string, len(ids)) // label id -> project slug
	unnamed := 0
	for _, id := range ids {
		p := s.legacy[id]
		if p.Slug != "" {
			// The definition is already there; the label's Name is the
			// ONLY copy of the display name ("My Web App" was slugified
			// to my-web-app when it was attached), so it has to be
			// written into project.yml BEFORE the table is dropped.
			if _, ok := s.Project(p.Slug); !ok {
				continue // its directory is gone: its sessions go unassigned
			}
			slugOf[id] = p.Slug
			if p.Name != "" && p.Name != p.Slug {
				if err := projectdef.SetName(s.home, p.Slug, p.Name); err != nil {
					fmt.Fprintln(os.Stderr, "bough serve: migrate project:", err)
				}
			}
			continue
		}
		// A label-only project becomes a definition with no repos.
		base := slugify(p.Name)
		if projectdef.ValidSlug(base) != nil {
			// Emoji, CJK or punctuation: nothing to name a directory after.
			unnamed++
			base = fmt.Sprintf("project-%d", unnamed)
		}
		if len(base) > 55 {
			base = strings.TrimRight(base[:55], "-") // leave room for a suffix
		}
		slug := base
		// Step past every slug that is spoken for, EXCEPT one an earlier
		// run of this migration left behind for this same label: that
		// one is finished rather than duplicated.
		for i := 2; taken[slug] || (onDisk[slug].Slug != "" && !ourLeftover(onDisk[slug], p.Name)); i++ {
			slug = fmt.Sprintf("%s-%d", base, i)
		}
		taken[slug] = true
		// A leftover is adopted as it stands: CreateEmpty already wrote
		// this name into it, so nothing here renames a directory that
		// this migration did not create.
		if _, err := projectdef.CreateEmpty(s.home, slug, p.Name); err != nil && !errors.Is(err, fs.ErrExist) {
			fmt.Fprintln(os.Stderr, "bough serve: migrate project:", err)
			continue
		}
		slugOf[id] = slug
	}

	for sid, m := range s.meta {
		if m.Project == "" {
			continue
		}
		if slug, ok := slugOf[m.Project]; ok {
			m.Project = slug
			s.meta[sid] = m
			continue
		}
		// Already a slug (this ran before and did not finish), or a label
		// that no longer resolves. Never leave a bogus slug behind.
		if _, ok := s.Project(m.Project); ok {
			continue
		}
		m.Project = ""
		s.meta[sid] = m
	}

	s.metaVersion, s.legacy = metaVersion, nil
	if !existed && len(ids) == 0 {
		// A fresh install: writing meta.json here would only create a
		// file holding a version number and nothing else.
		return nil
	}
	return s.saveMetaLocked()
}

// ourLeftover says whether a directory already under ~/.bough/projects
// is one an earlier run of migrateProjects made for a label of this
// name and then crashed before the table was dropped. CreateEmpty
// writes the name and `repos: []` and nothing else, so that shape —
// parsed, named the same, no repos — is ours to finish. Anything else
// is a project of the person's own, which must not be renamed or have
// another project's sessions filed into it.
func ourLeftover(e projectdef.Entry, name string) bool {
	return e.Err == nil && e.Def.Name == name && len(e.Def.Repos) == 0
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
	if err := json.Unmarshal(b, &f); err == nil && (f.Sessions != nil || f.Projects != nil || f.Version > 0) {
		for k, v := range f.Sessions {
			s.meta[k] = v
		}
		for slug, id := range f.Mains {
			s.mains[slug] = id
		}
		s.metaVersion = f.Version
		s.legacy = f.Projects
		s.requeueLocked()
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

// metaVersion is the schema meta.json is written at. 2 is "projects are
// directories": the label table is gone and SessionMeta.Project holds a
// slug. The version exists so a new binary skips an already-done
// migration in O(1) — running an OLD binary against a version-2 file is
// NOT supported, because its adoptDefinitions would mint fresh labels
// into the same file and the slugs would stop resolving.
const metaVersion = 2

// metaFile is what meta.json holds: the schema version and the
// supervisor's per-session metadata, in one atomically-written file.
type metaFile struct {
	Version  int                    `json:"version,omitempty"`
	Sessions map[string]SessionMeta `json:"sessions,omitempty"`
	// Mains is project slug -> main thread session id.
	Mains map[string]string `json:"mains,omitempty"`
	// Projects is the version-1 label table. It is read once, by
	// migrateProjects, and never written again.
	Projects map[string]oldProject `json:"projects,omitempty"`
}

// oldProject is one row of the version-1 label table: a minted id, a
// display name, and the definition directory it had been pointed at.
type oldProject struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Slug string `json:"slug,omitempty"`
}

// saveMetaLocked rewrites meta.json atomically; caller holds s.mu.
func (s *Supervisor) saveMetaLocked() error {
	if s.opt.MetaPath == "" {
		return nil
	}
	b, err := json.MarshalIndent(metaFile{Version: metaVersion, Sessions: s.meta, Mains: s.mains}, "", "  ")
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
	s.queue = nil
	for id, st := range s.deltas {
		if st.timer != nil {
			st.timer.Stop()
		}
		delete(s.deltas, id)
	}
	for id, subs := range s.subs {
		for key, cch := range subs {
			close(cch)
			delete(subs, key)
		}
		delete(s.subs, id)
	}
	return nil
}
