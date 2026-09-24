//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/connect"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/setup_key_env_precedence.fizz: where a provider key lives once
// serve runs, driven at the server. The adapter reads off the real
// system what it can: the lines of $HOME/.bough/env, what
// /api/setup/check answers (the provider is a local stub that accepts
// only the key issued last, and records which key it was asked about)
// and, for StartSession, the key a real session child has in its env
// (its first model response is a bash call printing it into the
// transcript). serve's process env has no reader, so the adapter keeps
// it (serveEnv) from what it did to serve; every check that reaches the
// stub and every session started holds that view to what serve really
// did. The shell, the key issued, and the requests in flight are the
// test's own.
//
// P is anthropic, Q is openai: the second writer of the same file.

const (
	skepP    = "ANTHROPIC_API_KEY"
	skepQ    = "OPENAI_API_KEY"
	skepKey  = "sk-ant-k" // + version: the key the provider issued n-th
	skepQKey = "sk-oa-q"
)

// skepProbe is the bash call a session's first response makes: the key
// in its env, between markers the command text itself cannot match.
const skepProbe = `printf 'PKEY[%s]\n' "$ANTHROPIC_API_KEY"`

var skepProbeOut = regexp.MustCompile(`PKEY\[` + skepKey + `(\d+)\]`)

type skepAdapter struct {
	t    *testing.T
	gate gate

	provider *httptest.Server
	mu       sync.Mutex
	issued   int    // the only key the provider accepts
	down     bool   // the provider does not answer (502)
	asked    string // the key the last check sent

	// A fresh serve per walk: nothing in the API takes a key back out of
	// serve's env.
	s     *servetest.Server
	dir   string // llm-control
	turns int

	serveEnv, shell, sess int
	pSaving, qSaving      bool
	qSaved                bool
	page                  string
	pageAbout, pageSaved  bool

	started []startedSession

	// acceptRevoked is TestSetupKeyEnvPrecedenceCatchesWrongAdapter's
	// deliberate bug: the stub accepts a key the provider revoked.
	acceptRevoked bool
}

func newSkepAdapter(t *testing.T) *skepAdapter {
	a := &skepAdapter{t: t}
	a.provider = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		a.mu.Lock()
		defer a.mu.Unlock()
		a.asked = r.Header.Get("x-api-key")
		switch {
		case a.down:
			w.WriteHeader(http.StatusBadGateway)
		case a.asked == skepKeyOf(a.issued) || a.acceptRevoked:
			w.WriteHeader(http.StatusOK)
		default:
			w.WriteHeader(http.StatusUnauthorized)
		}
	}))
	t.Cleanup(a.provider.Close)
	return a
}

func skepKeyOf(v int) string { return skepKey + strconv.Itoa(v) }

// skepVersion reads a key back as its version; 0 is no key.
func skepVersion(key string) (int, error) {
	if key == "" {
		return 0, nil
	}
	n, err := strconv.Atoi(strings.TrimPrefix(key, skepKey))
	if err != nil || !strings.HasPrefix(key, skepKey) {
		return 0, fmt.Errorf("a key this walk never issued: %q", key)
	}
	return n, nil
}

// skepEnv is serve's launching env: launchd's, with no key in it. The
// check goes to the stub; loopback is never proxied, so a check that
// slipped past it fails instead of leaving the machine.
func (a *skepAdapter) skepEnv(extra ...string) []string {
	return append([]string{
		"BOUGH_SETUP_CHECK_URL=" + a.provider.URL,
		"HTTPS_PROXY=http://127.0.0.1:9", "HTTP_PROXY=http://127.0.0.1:9",
	}, extra...)
}

func (a *skepAdapter) Init() error {
	if a.s != nil {
		a.s.Close()
	}
	a.s = servetest.Start(a.t, servetest.Options{Config: controlConfig, Env: a.skepEnv()})
	a.dir = control.Dir(a.s.Home)
	a.mu.Lock()
	a.issued, a.down, a.asked = 0, false, ""
	a.mu.Unlock()
	a.serveEnv, a.shell, a.sess = 0, 0, 0
	a.pSaving, a.qSaving, a.qSaved = false, false, false
	a.page, a.pageAbout, a.pageSaved = "", true, true
	a.gate.reset()
	return nil
}

func (a *skepAdapter) Cleanup() error { return nil }

func (a *skepAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Keys", Index: 0}: a}, nil
}

func (a *skepAdapter) envFile() string { return filepath.Join(a.s.Home, ".bough", "env") }

// skepFile is $HOME/.bough/env as the spec sees it.
type skepFile struct {
	exportP, plainP int
	fileQ           bool
}

func (f skepFile) first() int {
	if f.exportP != 0 {
		return f.exportP
	}
	return f.plainP
}

func (f skepFile) last() int {
	if f.plainP != 0 {
		return f.plainP
	}
	return f.exportP
}

// file reads the env file. Two lines of one form for P are a state the
// spec has no name for, so they are an error, not a guess.
func (a *skepAdapter) file() (skepFile, error) {
	var f skepFile
	b, err := os.ReadFile(a.envFile())
	if errors.Is(err, os.ErrNotExist) {
		return f, nil
	}
	if err != nil {
		return f, err
	}
	for line := range strings.SplitSeq(string(b), "\n") {
		line = strings.TrimSpace(line)
		exported := strings.HasPrefix(line, "export ")
		k, v, ok := strings.Cut(strings.TrimPrefix(line, "export "), "=")
		if !ok {
			continue
		}
		switch strings.TrimSpace(k) {
		case skepQ:
			f.fileQ = true
		case skepP:
			n, err := skepVersion(strings.Trim(strings.TrimSpace(v), `"'`))
			if err != nil {
				return f, err
			}
			slot := &f.plainP
			if exported {
				slot = &f.exportP
			}
			if *slot != 0 {
				return f, fmt.Errorf("the env file has two %s lines of one form:\n%s", skepP, b)
			}
			*slot = n
		}
	}
	return f, nil
}

// The spec's helpers, over the adapter's view.
func (a *skepAdapter) sessionKey(f skepFile) int {
	if a.serveEnv != 0 {
		return a.serveEnv
	}
	return f.first()
}

func (a *skepAdapter) checkKey(f skepFile) int {
	if a.serveEnv != 0 {
		return a.serveEnv
	}
	return f.last()
}

func (a *skepAdapter) stale(f skepFile) bool {
	return a.serveEnv != 0 && f.last() != 0 && a.serveEnv != f.last()
}

func (a *skepAdapter) GetState() (map[string]any, error) {
	f, err := a.file()
	if err != nil {
		return nil, err
	}
	a.mu.Lock()
	issued := a.issued
	a.mu.Unlock()
	return map[string]any{
		"exportP": f.exportP, "plainP": f.plainP, "fileQ": f.fileQ,
		"serveEnv": a.serveEnv, "shell": a.shell, "issued": issued,
		"pSaving": a.pSaving, "qSaving": a.qSaving, "qSaved": a.qSaved,
		"page": a.page, "pageAbout": a.pageAbout, "pageSaved": a.pageSaved,
		"sess": a.sess,
	}, nil
}

// issue has the provider mint the next key and revoke the one before.
func (a *skepAdapter) issue() (int, bool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !a.gate.pass(a.issued < 2 && !a.pSaving) {
		return 0, false
	}
	a.issued++
	return a.issued, true
}

func (a *skepAdapter) setKey(provider, key string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.API(ctx, http.MethodPost, "/api/setup/key", map[string]string{"provider": provider, "key": key}, nil)
}

// --- entering a key ------------------------------------------------------

// SaveKeyViaWelcome is the welcome's Save pressed; the POST is sent when
// it lands, so a check in between sees the file as it was.
func (a *skepAdapter) SaveKeyViaWelcome() error {
	if _, ok := a.issue(); ok {
		a.pSaving = true
	}
	return nil
}

func (a *skepAdapter) SaveKeyLands() error {
	if !a.gate.pass(a.pSaving) {
		return nil
	}
	a.mu.Lock()
	v := a.issued
	a.mu.Unlock()
	if err := a.setKey("anthropic", skepKeyOf(v)); err != nil {
		return err
	}
	a.serveEnv, a.pSaving = v, false
	return nil
}

// ConnectInTerminal is /connect in another bough process: the same
// writer, on the same file, and serve's env untouched.
func (a *skepAdapter) ConnectInTerminal() error {
	v, ok := a.issue()
	if !ok {
		return nil
	}
	return connect.WriteKey(a.envFile(), skepP, skepKeyOf(v))
}

// EditEnvFile is the person pasting the dotfile form over P's lines.
func (a *skepAdapter) EditEnvFile() error {
	v, ok := a.issue()
	if !ok {
		return nil
	}
	b, err := os.ReadFile(a.envFile())
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	var kept []string
	for line := range strings.SplitSeq(strings.TrimRight(string(b), "\n"), "\n") {
		k, _, _ := strings.Cut(strings.TrimPrefix(strings.TrimSpace(line), "export "), "=")
		if line != "" && strings.TrimSpace(k) != skepP {
			kept = append(kept, line)
		}
	}
	kept = append(kept, "export "+skepP+"="+skepKeyOf(v), "")
	return os.WriteFile(a.envFile(), []byte(strings.Join(kept, "\n")), 0o600)
}

func (a *skepAdapter) ExportInShell() error {
	a.mu.Lock()
	v := a.issued
	a.mu.Unlock()
	if a.gate.pass(v > 0 && a.shell != v) {
		a.shell = v
	}
	return nil
}

// --- the other writer ----------------------------------------------------

func (a *skepAdapter) ConcurrentSaveKey() error {
	if a.gate.pass(!a.qSaving && !a.qSaved) {
		a.qSaving = true
	}
	return nil
}

func (a *skepAdapter) ConcurrentSaveLands() error {
	if !a.gate.pass(a.qSaving) {
		return nil
	}
	if err := a.setKey("openai", skepQKey); err != nil {
		return err
	}
	a.qSaving, a.qSaved = false, true
	return nil
}

// --- reading the key -----------------------------------------------------

func (a *skepAdapter) CheckKey() error {
	a.gate.pass(true)
	return a.check(false)
}

func (a *skepAdapter) CheckKeyUnreachable() error {
	f, err := a.file()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.checkKey(f) != 0 && !a.stale(f)) {
		return nil
	}
	return a.check(true)
}

// check is GET /api/setup/check, read the way the welcome's key line
// reads it. A check that reached the provider must have asked about
// the key the adapter's view of serve says it would.
func (a *skepAdapter) check(down bool) error {
	if a.gate.off {
		return nil
	}
	f, err := a.file()
	if err != nil {
		return err
	}
	k := a.checkKey(f)
	a.mu.Lock()
	a.down, a.asked = down, ""
	a.mu.Unlock()
	var r struct {
		State string `json:"state"`
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.API(ctx, http.MethodGet, "/api/setup/check?provider=anthropic", nil, &r); err != nil {
		return err
	}
	a.mu.Lock()
	asked := a.asked
	a.down = false
	a.mu.Unlock()
	if asked != "" && asked != skepKeyOf(k) {
		return fmt.Errorf("the check asked the provider about %q; serve's env and the file say it would ask about key %d", asked, k)
	}
	switch r.State {
	case "ok":
		a.page = "works"
	case "unset", "unknown", "rejected", "stale":
		a.page = r.State
	default:
		return fmt.Errorf("/api/setup/check answered state %q", r.State)
	}
	sk := a.sessionKey(f)
	a.pageAbout = k == sk
	a.pageSaved = f.last() == 0 || sk == f.last()
	return nil
}

// StartSession is a real session: its first response prints the key in
// its env, its second ends the turn, and the transcript says which key
// it ran on.
func (a *skepAdapter) StartSession() error {
	f, err := a.file()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.sessionKey(f) != 0) {
		return nil
	}
	a.turns++
	name := fmt.Sprintf("s%05d", a.turns)
	control.Queue(a.t, a.dir, name+"a", control.Turn{Mode: "ok", Calls: []control.Call{{Name: "bash", Args: map[string]any{"command": skepProbe}}}})
	control.Queue(a.t, a.dir, name+"b", control.Turn{Mode: "ok", Text: "probed"})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Home, "which key "+name)
	if err != nil {
		return err
	}
	if _, err := waitRow(a.s, row.ID, "the probe turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", row.ID+".jsonl"))
	if err != nil {
		return err
	}
	v, ok := skepSessionKey(entries)
	if !ok {
		b, _ := json.Marshal(entries)
		return fmt.Errorf("session %s never printed its key: %s", row.ID, b)
	}
	a.started = append(a.started, startedSession{a.s.Home, row.ID})
	a.sess = v
	return nil
}

// skepSessionKey is the key version the probe printed; 0 is an empty env.
func skepSessionKey(entries []history.Entry) (int, bool) {
	for _, e := range entries {
		b, _ := json.Marshal(e.Data)
		if m := skepProbeOut.FindSubmatch(b); m != nil {
			n, _ := strconv.Atoi(string(m[1]))
			return n, true
		}
		if strings.Contains(string(b), "PKEY[]") {
			return 0, true
		}
	}
	return 0, false
}

// ServeRestart is launchd restarting serve: no key in its env, so the
// file's first line for P becomes serve's.
func (a *skepAdapter) ServeRestart() error {
	if !a.gate.pass(!a.pSaving && !a.qSaving) {
		return nil
	}
	f, err := a.file()
	if err != nil {
		return err
	}
	if err := a.restart(a.skepEnv()); err != nil {
		return err
	}
	a.serveEnv = f.first()
	return nil
}

func (a *skepAdapter) ServeRestartFromShell() error {
	if !a.gate.pass(a.shell != 0 && !a.pSaving && !a.qSaving) {
		return nil
	}
	if err := a.restart(a.skepEnv(skepP + "=" + skepKeyOf(a.shell))); err != nil {
		return err
	}
	a.serveEnv = a.shell
	return nil
}

func (a *skepAdapter) restart(env []string) error {
	a.s.Shutdown()
	a.s.SetEnv(env)
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.page, a.pageAbout, a.pageSaved = "", true, true
	return nil
}

var setupKeyEnvPrecedenceActions = map[string]map[string]fmbt.ActionFunc{"Keys": {
	"SaveKeyViaWelcome":     action((*skepAdapter).SaveKeyViaWelcome),
	"SaveKeyLands":          action((*skepAdapter).SaveKeyLands),
	"ConnectInTerminal":     action((*skepAdapter).ConnectInTerminal),
	"EditEnvFile":           action((*skepAdapter).EditEnvFile),
	"ExportInShell":         action((*skepAdapter).ExportInShell),
	"ConcurrentSaveKey":     action((*skepAdapter).ConcurrentSaveKey),
	"ConcurrentSaveLands":   action((*skepAdapter).ConcurrentSaveLands),
	"CheckKey":              action((*skepAdapter).CheckKey),
	"CheckKeyUnreachable":   action((*skepAdapter).CheckKeyUnreachable),
	"StartSession":          action((*skepAdapter).StartSession),
	"ServeRestart":          action((*skepAdapter).ServeRestart),
	"ServeRestartFromShell": action((*skepAdapter).ServeRestartFromShell),
}}

func setupKeyEnvPrecedenceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// setupKeyEnvPrecedenceHistory reads what a transcript can say: the key
// the session ran on. Keys leave nothing else in a transcript, so the
// trace is the shortest path to a session on that key, welcome saves
// only; a transcript that never printed its key is a bare Init claiming
// a session, which the check rejects.
func setupKeyEnvPrecedenceHistory(entries []history.Entry) []tracecheck.Step {
	v, ok := skepSessionKey(entries)
	if !ok || v == 0 {
		return []tracecheck.Step{{Action: "Init", State: map[string]any{"Keys#0.sess": 1}}}
	}
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Keys#0.sess": 0}}}
	for range v {
		steps = append(steps, tracecheck.Step{Action: "Keys#0.SaveKeyViaWelcome"}, tracecheck.Step{Action: "Keys#0.SaveKeyLands"})
	}
	return append(steps, tracecheck.Step{Action: "Keys#0.StartSession", State: map[string]any{"Keys#0.sess": v}})
}

func init() { historyProjections["setup_key_env_precedence"] = setupKeyEnvPrecedenceHistory }

// TestSetupKeyEnvPrecedence is the runner's random walks (the
// exhaustive run only; see runMBT).
func TestSetupKeyEnvPrecedence(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSkepAdapter(t)
	if err := runMBT(t, "setup_key_env_precedence", a, setupKeyEnvPrecedenceActions, setupKeyEnvPrecedenceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func skepFixtures() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "setup_key_env_precedence")
}

func skepPaths(t *testing.T, cover tracecheck.Cover) []swPath {
	t.Helper()
	raw, err := pathsJSONCover("setup_key_env_precedence", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []swPath `json:"paths"`
	}
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walk drives one path through a fresh serve and compares the adapter's
// state with the spec's at every node. Every action on a path is
// enabled, so a closed gate is the adapter misreading a require.
func (a *skepAdapter) walk(p swPath) error {
	for i, step := range p.Trace {
		name := strings.TrimPrefix(step.Action, "Keys#0.")
		var err error
		if i == 0 {
			err = a.Init()
		} else if f, ok := setupKeyEnvPrecedenceActions["Keys"][name]; ok {
			_, err = f(a, nil)
		} else {
			err = fmt.Errorf("no adapter action for %q", step.Action)
		}
		if err == nil && a.gate.off {
			err = errors.New("the adapter's gate closed on an action the spec enables")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Keys#0.")
			if !ok {
				continue
			}
			if !skepEqual(got[field], want) {
				return fmt.Errorf("step %d (%s): state mismatch for field %s: expected %v, actual %v", i, step.Action, field, want, got[field])
			}
		}
	}
	return nil
}

// skepEqual compares a Go value with one decoded from the graph, where
// every number is a float64.
func skepEqual(got, want any) bool {
	if n, ok := got.(int); ok {
		f, ok := want.(float64)
		return ok && float64(n) == f
	}
	return got == want
}

// TestSetupKeyEnvPrecedencePaths walks every path against a real serve,
// then replays each session's transcript on the graph.
func TestSetupKeyEnvPrecedencePaths(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(skepFixtures())
	if err != nil {
		t.Fatal(err)
	}
	for i, p := range skepPaths(t, envCover()) {
		t.Run(fmt.Sprintf("path%03d_to_%d", i, p.Target), func(t *testing.T) {
			t.Parallel()
			a := newSkepAdapter(t)
			if err := a.walk(p); err != nil {
				t.Fatal(err)
			}
			for _, s := range a.started {
				checkHistory(t, g, sessionHistory(t, s.home, s.id), setupKeyEnvPrecedenceHistory)
			}
		})
	}
}

// A stub that accepts a revoked key must fail the walk: the only state
// that tells "works" from "rejected" is a check while a save is still in
// flight, one transition, so the walks here take every link.
func TestSetupKeyEnvPrecedenceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	for _, p := range skepPaths(t, tracecheck.CoverTransitions) {
		i := slices.IndexFunc(p.Trace, func(s tracecheck.Step) bool { return s.State["Keys#0.page"] == "rejected" })
		if i < 0 {
			continue
		}
		a := newSkepAdapter(t)
		a.acceptRevoked = true
		err := a.walk(swPath{Trace: p.Trace[:i+1]})
		if err == nil {
			t.Fatal("a walk whose provider accepts a revoked key passed; the paths are not checking state")
		}
		t.Logf("caught, as it should be: %v", err)
		return
	}
	t.Fatal("no path has a key rejected")
}
