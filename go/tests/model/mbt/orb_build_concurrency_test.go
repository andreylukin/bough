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
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime/pprof"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_build_concurrency.fizz: two builders of one project's image
// racing through build.lock — serve's build goroutine (POST
// .../orb/build) and a session's child (orb.Prepare + orb.Start) — and
// the two views that follow them: the Orb panel's build log and the
// session's build log.
//
// Like orb_image_build, serve runs in process (serve.NewSupervisor +
// serve.NewAPI behind httptest), not through servetest: a `bough serve`
// process opens the host's container runtime, and this suite may never
// touch a real one. The child is not a process either but the real
// orb.Prepare/orb.Start in a goroutine of the test, on the same fake
// runtime, which is the only way two builders can share one fake.
//
// The spec's steps are the runtime calls each builder makes, held until
// the adapter answers them (obcRuntime):
//
//   - serve "sync": its first ImageExists, parked before it answers
//     (ServeSyncFail answers an error, which fails the build before
//     build.json as a SyncRepos failure does);
//   - "wait": a builder whose first check found no usable tag, parked
//     on its way to build.lock. The check was made when the step was
//     taken; the builder goes on to the lock only when the adapter lets
//     it (*Acquire), because in the product the lock is taken the moment
//     it is free and the spec's "wait, unlocked" is a real race window;
//   - "build": its Commit, holding build.lock;
//   - the child's "start": rt.Start of the tag EnsureImage returned.
//
// KillBuilder is SIGKILL, which a goroutine cannot receive: the killed
// child's call is never answered (it writes nothing again, so
// state.json and build.json stay as it left them), and when it held
// build.lock the lock is released on its descriptor, as the kernel
// releases a dead process's flock.

type obcKey struct{}

// obcBuilder is one EnsureImage caller as the runtime sees it: serve's
// goroutine (one per walk, reused by each POST) or one child run.
type obcBuilder struct {
	serve bool
	// next is which project-tag ImageExists this builder makes next:
	// "first" (before the lock), "second" (under it) or "retry" (orb.Start
	// asking whether its tag is gone after rt.Start failed).
	next       string
	pre        bool // the tag was usable under the lock: a Commit now is the dup ghost
	startFails int  // rt.Start calls that failed in this run
	park       *obcPark
	tag        string        // child: the tag its container runs (after a good rt.Start)
	done       chan struct{} // child: closed when orb.Start returned
	err        error         // child: what it returned
	dead       bool          // child: killed
}

type obcPark struct {
	kind string // sync | wait | build | start
	tag  string
	ch   chan obcReply
}

type obcReply struct {
	err  error
	kill bool
}

// obcRuntime is container.Fake with the builders' calls held (see the
// file comment). A failed Commit leaves its tag behind, the worst case
// the spec models ("bad"); the tags that came from a failed build are
// kept in failed, which is how imga/imgb tell good from bad.
type obcRuntime struct {
	*container.Fake
	home string

	mu      sync.Mutex
	slug    string
	serveB  *obcBuilder
	failed  map[string]bool
	dup     bool // a build began for a tag that was usable
	kbad    bool // EnsureImage handed rt.Start a tag build.json says failed
	stop    chan struct{}
	stopped bool
}

func (r *obcRuntime) prefix() string { return "bough-orb/" + r.slug + ":" }

// who is the builder a call comes from: the child's context carries it,
// serve's goroutine runs on context.Background(), and anything else (a
// page read, the list's orb summary) is nobody's and passes through.
func (r *obcRuntime) who(ctx context.Context) *obcBuilder {
	if b, ok := ctx.Value(obcKey{}).(*obcBuilder); ok {
		return b
	}
	if ctx == context.Background() {
		return r.serveB
	}
	return nil
}

func tagHash(tag string) string { return tag[strings.LastIndexByte(tag, ':')+1:] }

// usable is EnsureImage's early return for tag, read now.
func (r *obcRuntime) usable(tag string) bool {
	ok, _ := r.Fake.ImageExists(context.Background(), tag)
	return ok && !orb.FailedBuild(r.home, r.slug, tagHash(tag))
}

// park holds b's call until the adapter answers it. Called with r.mu
// held; returns with it held.
func (r *obcRuntime) park(b *obcBuilder, kind, tag string) obcReply {
	ch := make(chan obcReply)
	b.park = &obcPark{kind: kind, tag: tag, ch: ch}
	r.mu.Unlock()
	var rep obcReply
	select {
	case rep = <-ch:
	case <-r.stop:
		rep.err = errors.New("test over")
	}
	if rep.kill {
		<-r.stop // a dead builder never runs again
		rep = obcReply{err: errors.New("killed")}
	}
	r.mu.Lock()
	return rep
}

// release answers b's held call; false when it holds none.
func (r *obcRuntime) release(b *obcBuilder, rep obcReply) bool {
	r.mu.Lock()
	var p *obcPark
	if b != nil {
		p, b.park = b.park, nil
	}
	r.mu.Unlock()
	if p == nil {
		return false
	}
	p.ch <- rep
	return true
}

func (r *obcRuntime) ImageExists(ctx context.Context, tag string) (bool, error) {
	r.mu.Lock()
	b := r.who(ctx)
	if b == nil || !strings.HasPrefix(tag, r.prefix()) {
		r.mu.Unlock()
		return r.Fake.ImageExists(ctx, tag)
	}
	defer r.mu.Unlock()
	switch b.next {
	case "retry":
		b.next = "first"
	case "second":
		b.next = "first"
		b.pre = r.usable(tag)
	default:
		b.pre = false
		if b.serve {
			if rep := r.park(b, "sync", tag); rep.err != nil {
				return false, rep.err
			}
		}
		if r.usable(tag) {
			return true, nil
		}
		if rep := r.park(b, "wait", tag); rep.err != nil {
			return false, rep.err
		}
		b.next = "second"
		return false, nil
	}
	return r.Fake.ImageExists(ctx, tag)
}

func (r *obcRuntime) Commit(ctx context.Context, spec container.CommitSpec, log io.Writer) error {
	r.mu.Lock()
	b := r.who(ctx)
	if b == nil || !strings.HasPrefix(spec.Tag, r.prefix()) {
		r.mu.Unlock()
		return r.Fake.Commit(ctx, spec, log)
	}
	if b.pre {
		r.dup = true
	}
	rep := r.park(b, "build", spec.Tag)
	if rep.err != nil {
		r.Fake.AddImage(spec.Tag)
		r.failed[spec.Tag] = true
		r.mu.Unlock()
		fmt.Fprintf(log, "fake build of %s: %v\n", spec.Tag, rep.err)
		return rep.err
	}
	delete(r.failed, spec.Tag)
	r.mu.Unlock()
	return r.Fake.Commit(ctx, spec, log)
}

func (r *obcRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	r.mu.Lock()
	b := r.who(ctx)
	if b == nil || b.serve {
		r.mu.Unlock()
		return r.Fake.Start(ctx, spec)
	}
	if r.failed[spec.Image] && orb.FailedBuild(r.home, r.slug, tagHash(spec.Image)) {
		r.kbad = true
	}
	rep := r.park(b, "start", spec.Image)
	r.mu.Unlock()
	err := rep.err
	if err == nil {
		err = r.Fake.Start(ctx, spec)
	}
	r.mu.Lock()
	if err != nil {
		b.startFails++
		b.next = "retry"
	} else {
		b.tag = spec.Image
	}
	r.mu.Unlock()
	return err
}

// Command runs nothing: the orb's egress proxy asks the guest for its
// resolv.conf, and container.Fake would answer from the host's.
func (r *obcRuntime) Command(ctx context.Context, name string, opt container.ExecOptions, argv ...string) *exec.Cmd {
	cmd := exec.CommandContext(ctx, "true")
	cmd.Err = errors.New("orb_build_concurrency: no exec in the fake guest")
	return cmd
}

// obcAdapter is the fmbt.Model and the spec's Project role: it presses
// the panel's Build image and the session view's Rebuild, starts, kills
// and ends the session's child, and edits the definition.
type obcAdapter struct {
	t     *testing.T
	rt    *obcRuntime
	sup   *serve.Supervisor
	srv   *httptest.Server
	home  string
	label string // pprof label on serve's goroutines: see serveAlive
	sid   string // the one session whose orb the child starts
	gate  gate
	wg    sync.WaitGroup // every child run

	walk   int
	slug   string
	defs   map[string]string // "A"/"B" -> setup.sh
	hashes map[string]string // image hash -> "A"/"B"
	edits  int
	starts int
	killed bool
	child  *obcBuilder

	steps []tracecheck.Step
	walks [][]tracecheck.Step

	// childFailAsOK is the deliberate wiring bug the wrong-adapter test
	// injects: ChildBuildFail lets the child's build succeed.
	childFailAsOK bool
}

func newOBCAdapter(t *testing.T) *obcAdapter {
	home := t.TempDir()
	hist := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(hist, 0o755); err != nil {
		t.Fatal(err)
	}
	rt := &obcRuntime{Fake: container.NewFake(), home: home, failed: map[string]bool{}, stop: make(chan struct{})}
	rt.serveB = &obcBuilder{serve: true}
	// The base image exists, so a build is the project's Commit alone.
	rt.AddImage(projectdef.BaseTag())
	sup, err := serve.NewSupervisor(serve.Options{
		Exe: "/bin/true", HistDir: hist, Home: home, Runtime: rt,
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	a := &obcAdapter{t: t, rt: rt, sup: sup, home: home, sid: "obc0001"}
	a.label = fmt.Sprintf("%p", a)
	// Every goroutine the server starts inherits this label, serve's
	// build goroutine included: that is how the adapter tells whether
	// that goroutine still lives while the page's views cannot (a dead
	// builder's build.json reads "building" whether or not serve is).
	pprof.Do(context.Background(), pprof.Labels("obc", a.label), func(context.Context) {
		a.srv = httptest.NewServer(serve.NewAPI(sup))
	})
	b, _ := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": home, "mode": "project"}})
	if err := os.WriteFile(filepath.Join(hist, a.sid+".jsonl"), append(b, '\n'), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		// Every held call returns, the dead ones too, and nothing is
		// left writing into the temp dir when it is removed.
		rt.mu.Lock()
		rt.stopped = true
		close(rt.stop)
		rt.mu.Unlock()
		a.wg.Wait()
		waitUntil("serve's build goroutine to end", func() bool { return !a.serveAlive() })
		a.srv.Close()
		sup.Close()
	})
	return a
}

// serveAlive says whether serve's build goroutine for this adapter runs.
func (a *obcAdapter) serveAlive() bool {
	var buf bytes.Buffer
	pprof.Lookup("goroutine").WriteTo(&buf, 1)
	for _, rec := range strings.Split(buf.String(), "\n\n") {
		if strings.Contains(rec, `"obc":"`+a.label+`"`) && strings.Contains(rec, "serve.(*API).buildOrb.func") {
			return true
		}
	}
	return false
}

func (a *obcAdapter) do(method, path, body string) (int, map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var rdr io.Reader
	if body != "" {
		rdr = strings.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, rdr)
	if err != nil {
		return 0, nil, err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	var out map[string]any
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &out); err != nil {
			return resp.StatusCode, nil, fmt.Errorf("%s %s: %q is not JSON", method, path, raw)
		}
	}
	return resp.StatusCode, out, nil
}

func (a *obcAdapter) projectDir() string { return filepath.Join(projectdef.Root(a.home), a.slug) }

func (a *obcAdapter) writeDef(name string) error {
	return os.WriteFile(filepath.Join(a.projectDir(), projectdef.FileSetup), []byte(a.defs[name]), 0o644)
}

func (a *obcAdapter) hashNow() (string, error) {
	p, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return "", err
	}
	return projectdef.ImageHash(a.home, p)
}

// Init starts each walk on a new project (no build.json, no image) and
// the session's orb as a run that ended left it: state.json stopped, no
// container.
func (a *obcAdapter) Init() error {
	a.walk++
	name := fmt.Sprintf("c%04d", a.walk)
	code, body, err := a.do("POST", "/api/projects", `{"name":"`+name+`"}`)
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("create project %s = %d %v", name, code, body)
	}
	p, _ := body["project"].(map[string]any)
	a.slug, _ = p["slug"].(string)
	a.defs = map[string]string{"A": "# definition A\n", "B": "# definition B\n"}
	a.hashes = map[string]string{}
	for _, d := range []string{"B", "A"} {
		if err := a.writeDef(d); err != nil {
			return err
		}
		h, err := a.hashNow()
		if err != nil {
			return err
		}
		a.hashes[h] = d
	}
	if len(a.hashes) != 2 {
		return fmt.Errorf("definitions A and B hash alike: %v", a.hashes)
	}
	a.rt.mu.Lock()
	a.rt.slug = a.slug
	a.rt.serveB = &obcBuilder{serve: true}
	a.rt.failed = map[string]bool{}
	a.rt.dup, a.rt.kbad = false, false
	a.rt.mu.Unlock()
	a.rt.Fake.Remove(context.Background(), container.OrbName(a.sid))
	st, _ := json.Marshal(orb.State{Session: a.sid, Project: a.slug, Status: orb.StatusStopped,
		Container: container.OrbName(a.sid), UpdatedAt: time.Now().UTC()})
	if err := writeFileAtomic(filepath.Join(orb.Dir(a.home, a.sid), "state.json"), st); err != nil {
		return err
	}
	a.edits, a.starts, a.killed, a.child = 0, 0, false, nil
	a.gate.reset()
	s, err := a.GetState()
	if err != nil {
		return err
	}
	a.steps = []tracecheck.Step{{Action: "Init", State: qualify(s)}}
	return nil
}

// Cleanup ends every builder the walk left held (answering each held
// call with an error until none is left) and deletes the project.
func (a *obcAdapter) Cleanup() error {
	a.walks = append(a.walks, a.steps)
	a.steps = nil
	stop := obcReply{err: errors.New("walk over")}
	for i := 0; ; i++ {
		a.rt.release(a.rt.serveB, stop)
		if a.child != nil && !a.child.dead {
			a.rt.release(a.child, stop)
		}
		if err := a.settle(); err != nil {
			return err
		}
		if !a.serveAlive() && (a.child == nil || a.child.dead || a.childDone()) {
			break
		}
		if i == 20 {
			return errors.New("cleanup: builders still running after 20 rounds")
		}
	}
	code, body, err := a.do("DELETE", "/api/projects/"+a.slug, "")
	if err == nil && code != http.StatusOK {
		err = fmt.Errorf("delete project %s = %d %v", a.slug, code, body)
	}
	return err
}

func (a *obcAdapter) childDone() bool {
	select {
	case <-a.child.done:
		return true
	default:
		return false
	}
}

// settle waits until both builders are parked or gone, so every read is
// of a state and never of a builder between two holds.
func (a *obcAdapter) settle() error {
	return waitUntil("the builders to settle", func() bool {
		a.rt.mu.Lock()
		serveParked := a.rt.serveB.park != nil
		c := a.child
		childQuiet := c == nil || c.dead || c.park != nil
		a.rt.mu.Unlock()
		if c != nil && !childQuiet {
			childQuiet = a.childDone()
		}
		return childQuiet && (serveParked || !a.serveAlive())
	})
}

func (a *obcAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// obcFields is the role's state, read off the ground truth: the
// definition on disk, the fake's tags, build.json, where each builder
// is held, serve's buildErr (the panel's error) and the adapter's own
// counts of what a person did.
type obcFields struct {
	defn, imga, imgb, jst, jh, s, sh, k, kh string
	edits, starts                           int
	serr, kretry, killed, dup, kbad         bool
}

func (f obcFields) state() map[string]any {
	return map[string]any{
		"defn": f.defn, "edits": f.edits, "starts": f.starts, "imga": f.imga, "imgb": f.imgb,
		"jst": f.jst, "jh": f.jh, "s": f.s, "sh": f.sh, "serr": f.serr, "k": f.k, "kh": f.kh,
		"kretry": f.kretry, "killed": f.killed, "dup": f.dup, "kbad": f.kbad,
	}
}

// The spec's derived views, line for line.

func (f obcFields) locked() bool { return f.s == "build" || f.k == "build" }

func (f obcFields) panelState() string {
	st := f.jst
	if f.serr {
		st = "failed"
	}
	if f.s != "idle" {
		st = "building"
	}
	return st
}

func (f obcFields) sessionState() string {
	st := f.jst
	if f.s != "idle" {
		st = "building"
	} else if st == "building" && f.k != "wait" && f.k != "build" && f.k != "dead" {
		st = "interrupted"
	}
	return st
}

func (a *obcAdapter) name(hash string) (string, error) {
	if hash == "" {
		return "", nil
	}
	if d, ok := a.hashes[hash]; ok {
		return d, nil
	}
	return "", fmt.Errorf("hash %s is neither definition (%v)", hash, a.hashes)
}

// views is what the panel and the session's build view say, off the API.
type obcViews struct{ panel, panelErr, session string }

func (a *obcAdapter) views() (obcViews, error) {
	var v obcViews
	code, lb, err := a.do("GET", "/api/projects/"+a.slug+"/orb/build/log?offset=0", "")
	if err != nil {
		return v, err
	}
	if code != http.StatusOK {
		return v, fmt.Errorf("build log = %d %v", code, lb)
	}
	v.panel, _ = lb["state"].(string)
	v.panelErr, _ = lb["error"].(string)
	code, sb, err := a.do("GET", "/api/sessions/"+a.sid+"/orb/build/log?offset=0", "")
	if err != nil {
		return v, err
	}
	if code != http.StatusOK {
		return v, fmt.Errorf("session build log = %d %v", code, sb)
	}
	v.session, _ = sb["state"].(string)
	return v, nil
}

func (a *obcAdapter) observe() (obcFields, obcViews, error) {
	f := obcFields{edits: a.edits, starts: a.starts, killed: a.killed, s: "idle", k: "idle"}
	v, err := a.views()
	if err != nil {
		return f, v, err
	}
	f.serr = v.panelErr != ""
	h, err := a.hashNow()
	if err != nil {
		return f, v, err
	}
	if f.defn, err = a.name(h); err != nil {
		return f, v, err
	}
	if b, err := orb.ReadBuild(a.home, a.slug); err == nil {
		f.jst = b.State
		if f.jh, err = a.name(b.Hash); err != nil {
			return f, v, err
		}
	}
	serveAlive := a.serveAlive()
	r := a.rt
	r.mu.Lock()
	defer r.mu.Unlock()
	for d, img := range map[string]*string{"A": &f.imga, "B": &f.imgb} {
		*img = "none"
		for hash, name := range a.hashes {
			tag := projectdef.ImageTag(a.slug, hash)
			if ok, _ := r.Fake.ImageExists(context.Background(), tag); name == d && ok {
				*img = "good"
				if r.failed[tag] {
					*img = "bad"
				}
			}
		}
	}
	f.dup, f.kbad = r.dup, r.kbad
	if p := r.serveB.park; p != nil {
		f.s = p.kind
		if p.kind != "wait" {
			f.sh = a.hashes[tagHash(p.tag)]
		}
	} else if serveAlive {
		return f, v, errors.New("serve's build goroutine is between two holds")
	}
	if c := a.child; c != nil {
		switch {
		case c.dead:
			f.k = "dead"
		case c.park != nil:
			f.k = c.park.kind
			if c.park.kind != "wait" {
				f.kh = a.hashes[tagHash(c.park.tag)]
			}
			f.kretry = c.startFails > 0
		default:
			select {
			case <-c.done:
			default:
				return f, v, errors.New("the child is between two holds")
			}
			if st, _ := r.Fake.Inspect(context.Background(), container.OrbName(a.sid)); c.err == nil && st == container.StateRunning {
				f.k, f.kh = "up", a.hashes[tagHash(c.tag)]
			}
		}
	}
	return f, v, nil
}

// GetState is the role's fields; the panel's and the session view's
// states are the spec's derivations of them, and a view that says
// otherwise is an error on the step.
func (a *obcAdapter) GetState() (map[string]any, error) {
	f, v, err := a.observe()
	if err != nil {
		return nil, err
	}
	if want := f.panelState(); v.panel != want {
		return nil, fmt.Errorf("panel build log state %q, the spec's panel_state says %q (state %v)", v.panel, want, f.state())
	}
	if want := f.sessionState(); v.session != want {
		return nil, fmt.Errorf("session build log state %q, the spec's session_state says %q (state %v)", v.session, want, f.state())
	}
	return f.state(), nil
}

// step reads the adapter's view and runs do when require holds of it.
func (a *obcAdapter) step(require func(obcFields, obcViews) bool, do func(obcFields) error) error {
	f, v, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(f, v)) {
		return nil
	}
	if err := do(f); err != nil {
		return err
	}
	return a.settle()
}

// Edit saves the other definition, as the editor or an agent does.
func (a *obcAdapter) Edit() error {
	return a.step(func(f obcFields, _ obcViews) bool { return f.edits < 2 }, func(f obcFields) error {
		a.edits++
		if f.defn == "A" {
			return a.writeDef("B")
		}
		return a.writeDef("A")
	})
}

// post is the Build image and Rebuild buttons: the same endpoint.
func (a *obcAdapter) post(f obcFields) error {
	a.starts++
	want := http.StatusAccepted
	if f.s != "idle" {
		want = http.StatusConflict
	}
	code, body, err := a.do("POST", "/api/projects/"+a.slug+"/orb/build", `{}`)
	if err != nil {
		return err
	}
	if code != want {
		return fmt.Errorf("build = %d %v, want %d", code, body, want)
	}
	return nil
}

func (a *obcAdapter) BuildImage() error {
	return a.step(func(f obcFields, v obcViews) bool { return f.starts < 3 && v.panel != "building" }, a.post)
}

func (a *obcAdapter) Rebuild() error {
	return a.step(func(f obcFields, v obcViews) bool { return f.starts < 3 && v.session != "building" }, a.post)
}

func (a *obcAdapter) serveRelease(kind string, rep obcReply) error {
	return a.step(func(f obcFields, _ obcViews) bool {
		return f.s == kind && (kind != "wait" || !f.locked())
	}, func(obcFields) error {
		a.rt.release(a.rt.serveB, rep)
		return nil
	})
}

func (a *obcAdapter) ServeSyncFail() error {
	return a.serveRelease("sync", obcReply{err: errors.New("fake: clone failed")})
}
func (a *obcAdapter) ServeSynced() error  { return a.serveRelease("sync", obcReply{}) }
func (a *obcAdapter) ServeAcquire() error { return a.serveRelease("wait", obcReply{}) }
func (a *obcAdapter) ServeBuildOk() error { return a.serveRelease("build", obcReply{}) }
func (a *obcAdapter) ServeBuildFail() error {
	return a.serveRelease("build", obcReply{err: errors.New("fake: setup.sh exited 1")})
}

// ChildStart opens the session's orb the way its child does: Prepare,
// then Start, on the definition as it is now.
func (a *obcAdapter) ChildStart() error {
	return a.step(func(f obcFields, _ obcViews) bool {
		return f.starts < 3 && (f.k == "idle" || f.k == "dead")
	}, func(obcFields) error {
		a.starts++
		p, err := projectdef.Load(a.home, a.slug)
		if err != nil {
			return err
		}
		b := &obcBuilder{next: "first", done: make(chan struct{})}
		a.rt.mu.Lock()
		stopped := a.rt.stopped
		a.rt.mu.Unlock()
		if stopped {
			return errors.New("test over")
		}
		a.child = b
		ctx := context.WithValue(context.Background(), obcKey{}, b)
		rt, home, sid := a.rt, a.home, a.sid
		a.wg.Add(1)
		go func() {
			defer a.wg.Done()
			defer close(b.done)
			o, err := orb.Prepare(ctx, rt, home, sid, p, "")
			if err == nil {
				err = o.Start(ctx)
			}
			rt.mu.Lock()
			b.err = err
			rt.mu.Unlock()
		}()
		return nil
	})
}

func (a *obcAdapter) childRelease(kind string, rep obcReply) error {
	return a.step(func(f obcFields, _ obcViews) bool {
		return f.k == kind && (kind != "wait" || !f.locked())
	}, func(obcFields) error {
		a.rt.release(a.child, rep)
		return nil
	})
}

func (a *obcAdapter) ChildAcquire() error { return a.childRelease("wait", obcReply{}) }
func (a *obcAdapter) ChildBuildOk() error { return a.childRelease("build", obcReply{}) }
func (a *obcAdapter) ChildRun() error     { return a.childRelease("start", obcReply{}) }

func (a *obcAdapter) ChildBuildFail() error {
	rep := obcReply{err: errors.New("fake: setup.sh exited 1")}
	if a.childFailAsOK {
		rep = obcReply{}
	}
	return a.childRelease("build", rep)
}

// ChildExit ends the session: its container goes, and with it the pin
// that kept prune off its tag. state.json stays, so the session keeps
// its build view.
func (a *obcAdapter) ChildExit() error {
	return a.step(func(f obcFields, _ obcViews) bool { return f.k == "up" }, func(obcFields) error {
		return a.rt.Fake.Remove(context.Background(), container.OrbName(a.sid))
	})
}

// KillBuilder: the child's held call is never answered, and a flock it
// held is released on its descriptor, as the kernel does for a dead
// process.
func (a *obcAdapter) KillBuilder() error {
	return a.step(func(f obcFields, _ obcViews) bool {
		return !f.killed && (f.k == "wait" || f.k == "build")
	}, func(f obcFields) error {
		a.killed = true
		a.rt.mu.Lock()
		a.child.dead = true
		a.rt.mu.Unlock()
		a.rt.release(a.child, obcReply{kill: true})
		if f.k == "build" {
			return unlockHeld(filepath.Join(filepath.Dir(orb.ImageLogPath(a.home, a.slug)), "build.lock"))
		}
		return nil
	})
}

// unlockHeld releases the flock this process holds on path through
// whichever descriptor holds it.
func unlockHeld(path string) error {
	fi, err := os.Stat(path)
	if err != nil {
		return err
	}
	want := fi.Sys().(*syscall.Stat_t)
	ents, err := os.ReadDir("/dev/fd")
	if err != nil {
		return err
	}
	n := 0
	for _, e := range ents {
		fd, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		var st syscall.Stat_t
		if syscall.Fstat(fd, &st) == nil && st.Dev == want.Dev && st.Ino == want.Ino {
			if syscall.Flock(fd, syscall.LOCK_UN) == nil {
				n++
			}
		}
	}
	if n == 0 {
		return fmt.Errorf("no descriptor holds %s", path)
	}
	return nil
}

// obcRecorded writes down every step the gate let through with the
// state read after it: the walk as the server lived it, replayed on the
// graph by the tests below.
func obcRecorded(name string, f func(*obcAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*obcAdapter)
		if err := f(a); err != nil || a.gate.off {
			return nil, err
		}
		st, err := a.GetState()
		if err != nil {
			return nil, err
		}
		a.steps = append(a.steps, tracecheck.Step{Action: "Project#0." + name, State: qualify(st)})
		return nil, nil
	}
}

var obcActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"Edit":           obcRecorded("Edit", (*obcAdapter).Edit),
	"BuildImage":     obcRecorded("BuildImage", (*obcAdapter).BuildImage),
	"Rebuild":        obcRecorded("Rebuild", (*obcAdapter).Rebuild),
	"ServeSyncFail":  obcRecorded("ServeSyncFail", (*obcAdapter).ServeSyncFail),
	"ServeSynced":    obcRecorded("ServeSynced", (*obcAdapter).ServeSynced),
	"ServeAcquire":   obcRecorded("ServeAcquire", (*obcAdapter).ServeAcquire),
	"ServeBuildOk":   obcRecorded("ServeBuildOk", (*obcAdapter).ServeBuildOk),
	"ServeBuildFail": obcRecorded("ServeBuildFail", (*obcAdapter).ServeBuildFail),
	"ChildStart":     obcRecorded("ChildStart", (*obcAdapter).ChildStart),
	"ChildAcquire":   obcRecorded("ChildAcquire", (*obcAdapter).ChildAcquire),
	"ChildBuildOk":   obcRecorded("ChildBuildOk", (*obcAdapter).ChildBuildOk),
	"ChildBuildFail": obcRecorded("ChildBuildFail", (*obcAdapter).ChildBuildFail),
	"ChildRun":       obcRecorded("ChildRun", (*obcAdapter).ChildRun),
	"ChildExit":      obcRecorded("ChildExit", (*obcAdapter).ChildExit),
	"KillBuilder":    obcRecorded("KillBuilder", (*obcAdapter).KillBuilder),
}, "": {
	// With deadlock detection off, fizz links a state with nothing left
	// to do to itself as "end", and the runner offers it as a role-less
	// action (a missing one is a nil-pointer panic in the library). It
	// matches no link the runner checks, so it closes the gate like any
	// disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*obcAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func obcOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// obcHistory is what a session's transcript can say about this flow:
// nothing past its start. The builds live in build.json, state.json and
// serve's memory, not in history, so the walk journal (a.walks) is this
// flow's trace, replayed on the graph by every test here.
func obcHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.k": "idle", "Project#0.s": "idle"}}}
}

func init() { historyProjections["orb_build_concurrency"] = obcHistory }

// checkJournal replays every walk the adapter recorded on the graph.
func (a *obcAdapter) checkJournal(t *testing.T, g *tracecheck.Graph) {
	t.Helper()
	steps := 0
	for i, w := range a.walks {
		if v := g.Check(w); v != nil {
			b, _ := json.Marshal(w)
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
		steps += len(w) - 1
	}
	t.Logf("%d walks, %d steps replayed on the graph", len(a.walks), steps)
}

func TestOrbBuildConcurrency(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOBCAdapter(t)
	if err := runMBT(t, "orb_build_concurrency", a, obcActions, obcOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "orb_build_concurrency"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkJournal(t, g)
	checkHistory(t, g, sessionHistory(t, a.home, a.sid), obcHistory)
}

// walkOBCPaths drives the adapter down every walk of b and returns the
// first step whose state is not the spec's.
func walkOBCPaths(t *testing.T, a *obcAdapter, b []byte) error {
	t.Helper()
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no paths")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		for si, s := range p.Trace {
			if si > 0 {
				if s.Action == "end" {
					continue // nothing is left to do: the state is the last one
				}
				name := strings.TrimPrefix(s.Action, "Project#0.")
				f := obcActions["Project"][name]
				if f == nil {
					return fmt.Errorf("path %d step %d: no adapter action for %s", pi, si, s.Action)
				}
				if _, err := f(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, s.Action, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d (%s): the adapter's view says it is not enabled", pi, si, s.Action)
				}
			}
			// Init and every recorded action read the state right after
			// the step; the journal's last entry is that reading.
			got := a.steps[len(a.steps)-1].State
			if diff := stateDiff(s.State, got); diff != "" {
				return fmt.Errorf("path %d step %d (%s): %s", pi, si, s.Action, diff)
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d: Cleanup: %w", pi, err)
		}
	}
	return nil
}

// Every settled state (every transition under MODEL_COVER=transitions)
// against the real serve and orb code.
func TestOrbBuildConcurrencyPaths(t *testing.T) {
	t.Parallel()
	b, err := pathsJSON("orb_build_concurrency")
	if err != nil {
		t.Fatal(err)
	}
	a := newOBCAdapter(t)
	if err := walkOBCPaths(t, a, b); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "orb_build_concurrency"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkJournal(t, g)
	checkHistory(t, g, sessionHistory(t, a.home, a.sid), obcHistory)
}

// The walks prove nothing unless a wrong server fails them: here the
// child's failed build succeeds. It shows on ChildBuildFail alone, so
// the walk takes every transition to be sure to take that one.
func TestOrbBuildConcurrencyPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover("orb_build_concurrency", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	a := newOBCAdapter(t)
	a.childFailAsOK = true
	err = walkOBCPaths(t, a, b)
	if err == nil {
		t.Fatal("walks whose ChildBuildFail succeeds passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
