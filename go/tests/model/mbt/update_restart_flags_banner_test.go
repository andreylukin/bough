//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servepid"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/update_restart_flags_banner.fizz against the real commands: the
// room is installed with `bough serve --host=room.test <addr>` (detached,
// BOUGH_NO_LAUNCHD=1, so the restart takes launchServe's path; the
// launchd path passes the same flags to installServeAgent), and an
// update is `bough restart` run from a new hard link of the binary,
// which is a new build (buildID names the executable's path).
//
// The page is a client: its requests go to the serve's port with
// Host: room.test, the name a proxy in front of the loopback bind
// forwards. What the page does with the answers (baseline, banner,
// dismissal) is the adapter's copy of the spec's page, fed the builds
// and statuses the real serve answered; the browser stage checks
// api.ts's own copy.

const urfbHost = "room.test"

type updateRestartFlagsBannerAdapter struct {
	t    *testing.T
	s    *servetest.Server
	bin  string // build 1: the binary every walk installs
	gate gate

	// builds are the build ids this walk's serves answered with, in the
	// order they first answered: build n is builds[n-1].
	builds    []string
	installed int // binaries put in place this walk (the spec's build)
	up        bool
	flags     string // last read off the serve's argv
	links     int

	// parked is the serve `bough restart` launched, held before it
	// listens (see BoughUpdate) until LaunchdReinstall; 0 when none.
	parked int
	token  string

	page                           string
	bundle, baseline, seen, dismis int
	answer                         string
	banner                         bool

	ids   []string
	taken map[string]int

	// oldBinary is TestUpdateRestartFlagsBannerCatchesWrongAdapter's bug:
	// the update restarts the binary already running, so no new build.
	oldBinary bool
}

func newUpdateRestartFlagsBannerAdapter(t *testing.T) *updateRestartFlagsBannerAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	// servetest's serve set up HOME, the token and a free port; the room
	// is the detached daemon Init installs on the same port.
	s.Shutdown()
	a := &updateRestartFlagsBannerAdapter{t: t, s: s, bin: s.Bin(), taken: map[string]int{}}
	// Registered after servetest's, so it runs first: the daemon is gone
	// before its HOME is removed.
	t.Cleanup(func() {
		a.stopRoom()
		if t.Failed() {
			b, _ := os.ReadFile(filepath.Join(s.Home, ".bough", "serve.log"))
			t.Logf("serve.log:\n%s", b)
		}
	})
	return a
}

func (a *updateRestartFlagsBannerAdapter) env() []string {
	var env []string
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if k == "HOME" || k == "BOUGH_WEB_ADDR" || k == "BOUGH_BIN" || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		env = append(env, kv)
	}
	return append(env, "HOME="+a.s.Home, "BOUGH_WEB_ADDR=127.0.0.1:0", "BOUGH_NO_LAUNCHD=1")
}

func (a *updateRestartFlagsBannerAdapter) command(bin string, args ...string) *exec.Cmd {
	cmd := exec.Command(bin, args...)
	cmd.Dir = a.s.Home
	cmd.Env = a.env()
	return cmd
}

// servePID is the pid the serve pidfile names, 0 when none.
func (a *updateRestartFlagsBannerAdapter) servePID() int {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve.pid"))
	if err != nil {
		return 0
	}
	pid, _, _, _, _, err := servepid.Parse(string(b))
	if err != nil {
		return 0
	}
	return pid
}

// stopRoom ends whatever this walk left: a parked serve (killed, and
// the token file put back) and the daemon, with its session children
// (detached, the daemon leads its own process group).
func (a *updateRestartFlagsBannerAdapter) stopRoom() {
	if a.parked != 0 {
		syscall.Kill(a.parked, syscall.SIGKILL)
		for servepid.Alive(a.parked) {
			time.Sleep(20 * time.Millisecond)
		}
		a.parked = 0
		a.putToken()
	}
	pid := a.servePID()
	if pid == 0 || !servepid.Alive(pid) {
		return
	}
	syscall.Kill(pid, syscall.SIGTERM)
	deadline := time.Now().Add(5 * time.Second)
	for servepid.Alive(pid) && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	syscall.Kill(-pid, syscall.SIGKILL)
	for servepid.Alive(pid) {
		time.Sleep(20 * time.Millisecond)
	}
}

// Init installs a fresh room on build 1 and starts the room's one
// session through the proxy name.
func (a *updateRestartFlagsBannerAdapter) Init() error {
	a.gate.reset()
	a.stopRoom()
	out, err := a.command(a.bin, "serve", "--host="+urfbHost, a.s.Addr).CombinedOutput()
	if err != nil {
		return fmt.Errorf("bough serve --host=%s %s: %v: %s", urfbHost, a.s.Addr, err, out)
	}
	a.builds, a.installed, a.up, a.flags = nil, 1, true, "install"
	a.page, a.bundle, a.baseline, a.seen, a.dismis, a.answer, a.banner = "none", 0, 0, 0, 0, "none", false
	if _, err := a.readServe(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var r struct {
		Session serve.Row `json:"session"`
	}
	st, _, err := a.proxied(ctx, http.MethodPost, "/api/sessions", map[string]string{"cwd": a.s.Dir(a.t, "work"), "prompt": "the room's session"}, &r)
	if err != nil {
		return err
	}
	if st != http.StatusOK && st != http.StatusCreated {
		return fmt.Errorf("creating the room's session through %s: %d", urfbHost, st)
	}
	a.ids = append(a.ids, r.Session.ID)
	_, err = waitRow(a.s, r.Session.ID, "the session's turn", func(r serve.Row) bool { return r.Status == serve.StatusDone })
	return err
}

// proxied sends a request the way the page does behind the proxy: to
// the loopback port, with the proxy's Host.
func (a *updateRestartFlagsBannerAdapter) proxied(ctx context.Context, method, path string, body, out any) (int, string, error) {
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, "", err
	}
	req.Host = urfbHost
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if out != nil && resp.StatusCode/100 == 2 {
		if err := json.Unmarshal(raw, out); err != nil {
			return 0, "", fmt.Errorf("%s %s: %w: %s", method, path, err, raw)
		}
	}
	return resp.StatusCode, resp.Header.Get(serve.BuildHeader), nil
}

// buildNum numbers a build id in the order this walk first saw it.
func (a *updateRestartFlagsBannerAdapter) buildNum(id string) int {
	for i, b := range a.builds {
		if b == id {
			return i + 1
		}
	}
	a.builds = append(a.builds, id)
	return len(a.builds)
}

// readServe asks the serve on loopback which build it runs, and reads
// the flags it runs with off its argv: the pidfile's pid, as ps shows
// it. build is 0 when nothing answered in time.
func (a *updateRestartFlagsBannerAdapter) readServe() (build int, err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 700*time.Millisecond)
	defer cancel()
	id, err := a.s.Build(ctx)
	if err != nil {
		return 0, nil
	}
	build = a.buildNum(id)
	pid := a.servePID()
	out, err := exec.Command("ps", "-o", "command=", "-p", fmt.Sprint(pid)).Output()
	if err != nil {
		return 0, fmt.Errorf("ps %d: %w", pid, err)
	}
	args := strings.Fields(string(out))
	has := map[string]bool{}
	for _, f := range args {
		has[f] = true
	}
	a.flags = "lost"
	if has["--run"] && has[a.s.Addr] && has["--host="+urfbHost] && !has["--insecure-bind"] {
		a.flags = "install"
	}
	return build, nil
}

func (a *updateRestartFlagsBannerAdapter) Cleanup() error { return nil }

func (a *updateRestartFlagsBannerAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Room", Index: 0}: a}, nil
}

// GetState reads serve, build and flags off the running processes; the
// page's fields are the page's own.
func (a *updateRestartFlagsBannerAdapter) GetState() (map[string]any, error) {
	st := map[string]any{
		"page": a.page, "bundle": a.bundle, "baseline": a.baseline, "seen": a.seen,
		"answer": a.answer, "banner": a.banner, "dismissed": a.dismis,
		"serve": "down", "build": a.installed,
	}
	build, err := a.readServe()
	if err != nil {
		return nil, err
	}
	if build != 0 {
		st["serve"], st["build"] = "up", build
	}
	st["flags"] = a.flags
	return st, nil
}

// LoadPage fetches the control room's HTML through the proxy: the
// bundle is the build that served it.
func (a *updateRestartFlagsBannerAdapter) LoadPage() error {
	if !a.gate.pass(a.up && a.page == "none") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, id, err := a.proxied(ctx, http.MethodGet, "/", nil, nil)
	if err != nil {
		return err
	}
	a.page, a.bundle, a.baseline, a.seen, a.answer = "open", a.buildNum(id), 0, 0, "none"
	return nil
}

func (a *updateRestartFlagsBannerAdapter) tokenPath() string {
	return filepath.Join(a.s.Home, ".bough", "serve.token")
}

// putToken puts the token file back in place of the FIFO.
func (a *updateRestartFlagsBannerAdapter) putToken() {
	p := a.tokenPath()
	os.Remove(p)
	os.WriteFile(p, []byte(a.token+"\n"), 0o600)
}

// BoughUpdate puts a new binary in place and runs `bough restart` from
// it, which stops the room and launches the new serve. The spec splits
// that into the stop and the start because a page can ask in between;
// to hold the room there, serve.token is a FIFO while it runs: the new
// serve's first step (LoadToken) blocks opening it, before its pidfile
// or its port, so nothing answers until LaunchdReinstall writes the
// token. The restart itself runs whole, as a person runs it.
func (a *updateRestartFlagsBannerAdapter) BoughUpdate() error {
	if !a.gate.pass(a.up && a.installed < 3) {
		return nil
	}
	bin := a.bin
	if !a.oldBinary {
		a.links++
		bin = filepath.Join(a.s.Root, fmt.Sprintf("bough-u%d", a.links))
		if err := os.Link(a.bin, bin); err != nil {
			return err
		}
	}
	a.token = a.s.Token
	if err := os.Remove(a.tokenPath()); err != nil {
		return err
	}
	if err := syscall.Mkfifo(a.tokenPath(), 0o600); err != nil {
		a.putToken()
		return err
	}
	a.installed++
	a.up = false
	out, err := a.command(bin, "restart").CombinedOutput()
	if err != nil {
		a.putToken()
		return fmt.Errorf("bough restart: %v: %s", err, out)
	}
	if _, err := fmt.Sscanf(afterStr(string(out), "restarted control room on "+a.s.Addr+" (pid "), "%d", &a.parked); err != nil {
		a.putToken()
		return fmt.Errorf("bough restart named no new serve: %s", out)
	}
	return nil
}

func afterStr(s, sep string) string {
	_, after, _ := strings.Cut(s, sep)
	return after
}

// LaunchdReinstall lets the serve the restart launched go on: it reads
// the token, and the file is a file again for everyone after it.
func (a *updateRestartFlagsBannerAdapter) LaunchdReinstall() error {
	if !a.gate.pass(!a.up) {
		return nil
	}
	wrote := make(chan error, 1)
	go func() {
		f, err := os.OpenFile(a.tokenPath(), os.O_WRONLY, 0)
		if err == nil {
			_, err = f.WriteString(a.token + "\n")
			f.Close()
		}
		wrote <- err
	}()
	select {
	case err := <-wrote:
		if err != nil {
			return err
		}
	case <-time.After(actionTimeout):
		// Unblock the open, so the goroutine ends.
		if r, err := os.OpenFile(a.tokenPath(), os.O_RDONLY|syscall.O_NONBLOCK, 0); err == nil {
			r.Close()
		}
		return fmt.Errorf("the restarted serve (pid %d) never read the token", a.parked)
	}
	a.putToken()
	a.parked = 0
	a.up = true
	ctx, cancel := actionCtx()
	defer cancel()
	for {
		if _, err := a.s.Build(ctx); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return errors.New("the restarted serve never answered")
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// ApiAnswer is the page's list poll through the proxy; what the page
// does with the answer's build is the spec's.
func (a *updateRestartFlagsBannerAdapter) ApiAnswer() error {
	if !a.gate.pass(a.up && a.page == "open") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	st, id, err := a.proxied(ctx, http.MethodGet, "/api/sessions", nil, nil)
	if err != nil {
		return err
	}
	a.answer = "refused"
	if st == http.StatusOK {
		a.answer = "ok"
	}
	a.seen = a.buildNum(id)
	if a.baseline == 0 {
		a.baseline = a.bundle
	}
	if a.seen != a.baseline && a.dismis != a.seen {
		a.banner = true
	}
	return nil
}

func (a *updateRestartFlagsBannerAdapter) Dismiss() error {
	if a.gate.pass(a.banner) {
		a.banner, a.dismis = false, a.seen
	}
	return nil
}

// Reload loads the page again through the proxy.
func (a *updateRestartFlagsBannerAdapter) Reload() error {
	if !a.gate.pass(a.up && a.page == "open") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, id, err := a.proxied(ctx, http.MethodGet, "/", nil, nil)
	if err != nil {
		return err
	}
	a.bundle, a.baseline, a.seen, a.answer, a.banner, a.dismis = a.buildNum(id), 0, 0, "none", false, 0
	return nil
}

func urfbAction(name string, f func(*updateRestartFlagsBannerAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*updateRestartFlagsBannerAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
		}
		return nil, err
	}
}

var updateRestartFlagsBannerActions = map[string]map[string]fmbt.ActionFunc{"Room": {
	"LoadPage":         urfbAction("LoadPage", (*updateRestartFlagsBannerAdapter).LoadPage),
	"BoughUpdate":      urfbAction("BoughUpdate", (*updateRestartFlagsBannerAdapter).BoughUpdate),
	"LaunchdReinstall": urfbAction("LaunchdReinstall", (*updateRestartFlagsBannerAdapter).LaunchdReinstall),
	"ApiAnswer":        urfbAction("ApiAnswer", (*updateRestartFlagsBannerAdapter).ApiAnswer),
	"Dismiss":          urfbAction("Dismiss", (*updateRestartFlagsBannerAdapter).Dismiss),
	"Reload":           urfbAction("Reload", (*updateRestartFlagsBannerAdapter).Reload),
}}

// Every Init reinstalls the room and starts a session (a second or
// two); a walk that updates twice restarts serve twice more.
func updateRestartFlagsBannerOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 8, "max-parallel-runs": 0}
}

// updateRestartFlagsBannerHistory reads the room's session: it was
// started through the proxy name, so its first input is a request the
// installed serve answered "ok" for the page (Init, LoadPage, its first
// ApiAnswer on build 1). A transcript with no input is a create that
// never ran: a refusal, which the spec does not allow a proxied
// install. Updates and the banner leave nothing in a transcript.
func updateRestartFlagsBannerHistory(entries []history.Entry) []tracecheck.Step {
	answer := "refused"
	for _, e := range entries {
		if e.Kind == "input" {
			answer = "ok"
			break
		}
	}
	return []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Room#0.serve": "up", "Room#0.flags": "install", "Room#0.page": "none"}},
		{Action: "Room#0.LoadPage", State: map[string]any{"Room#0.page": "open", "Room#0.bundle": 1}},
		{Action: "Room#0.ApiAnswer", State: map[string]any{"Room#0.answer": answer, "Room#0.seen": 1, "Room#0.baseline": 1}},
	}
}

func init() { historyProjections["update_restart_flags_banner"] = updateRestartFlagsBannerHistory }

func TestUpdateRestartFlagsBanner(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newUpdateRestartFlagsBannerAdapter(t)
	if err := runMBT(t, "update_restart_flags_banner", a, updateRestartFlagsBannerActions, updateRestartFlagsBannerOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkUpdateRestartFlagsBannerHistories(t, a)
}

// TestUpdateRestartFlagsBannerPaths walks the paths the browser stage
// walks (every state; every link under MODEL_COVER=transitions), where
// the random walks rarely update twice.
func TestUpdateRestartFlagsBannerPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newUpdateRestartFlagsBannerAdapter(t)
	if err := walkUpdateRestartFlagsBannerPaths(a, envCover()); err != nil {
		t.Fatal(err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkUpdateRestartFlagsBannerHistories(t, a)
}

func walkUpdateRestartFlagsBannerPaths(a *updateRestartFlagsBannerAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("update_restart_flags_banner", cover)
	if err != nil {
		return err
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return err
	}
	for i, p := range f.Paths {
		for j, step := range p.Trace {
			if j == 0 {
				err = a.Init()
			} else {
				name := strings.TrimPrefix(step.Action, "Room#0.")
				_, err = updateRestartFlagsBannerActions["Room"][name](a, nil)
				if err == nil && a.gate.off {
					err = errors.New("the adapter found it disabled")
				}
			}
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): %w", i, j, step.Action, err)
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d (%s): state: %w", i, j, step.Action, err)
			}
			if d := roomDiff(step.State, got); d != "" {
				return fmt.Errorf("path %d step %d (%s): %s", i, j, step.Action, d)
			}
		}
	}
	return nil
}

func checkUpdateRestartFlagsBannerHistories(t *testing.T, a *updateRestartFlagsBannerAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "update_restart_flags_banner"))
	if err != nil {
		t.Fatal(err)
	}
	if len(a.ids) == 0 {
		t.Fatal("no session transcripts to check")
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), updateRestartFlagsBannerHistory)
	}
}

// The projection takes a transcript with its turn as the proxied
// install answering, and one whose create never ran as a refusal, which
// the spec does not allow.
func TestUpdateRestartFlagsBannerHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "update_restart_flags_banner"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	if v := g.Check(updateRestartFlagsBannerHistory([]history.Entry{e("meta"), e("input"), e("done")})); v != nil {
		t.Fatalf("the room's session: %v", v)
	}
	if v := g.Check(updateRestartFlagsBannerHistory([]history.Entry{e("meta")})); v == nil {
		t.Fatal("a session whose create never ran passed as a proxied install answering")
	}
}

// An update that restarts the binary already running (no new build)
// must fail: otherwise the walks are not reading the build that answers.
func TestUpdateRestartFlagsBannerCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newUpdateRestartFlagsBannerAdapter(t)
	a.oldBinary = true
	if err := walkUpdateRestartFlagsBannerPaths(a, tracecheck.CoverStates); err == nil {
		t.Fatal("walks whose update restarts the old binary passed; the adapter is not reading the build")
	} else {
		t.Logf("caught: %v", err)
	}
}
