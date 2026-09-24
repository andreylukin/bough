//go:build !windows

package mbt

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/serve_close_children_orbs.fizz against serve going down and
// coming back: one parent P, three background agents with max_running
// 1 (A running in a project orb, B then C queued behind it), sup.Close
// or a crash, and the next boot on the same HOME.
//
// Not through servetest, for two reasons the spec's notes give: a
// `bough serve` process uses the host's container runtime, which this
// suite may never touch, and "closing" (inside sup.Close, after
// srv.Shutdown) cannot be seen over HTTP. So serve runs in process, as
// in orb_lifecycle_test.go: the Supervisor and API `bough serve` builds,
// behind an httptest server, with container.Fake as the runtime. The
// Fake outlives a restart, as the host's runtime does; a boot is a new
// Supervisor on the same HOME. A crash is Supervisor.Crash: the children
// die and the supervisor does nothing more.
//
// The agents are real processes the supervisor spawns: this test binary
// re-executed as a stub session child (sccoStub). The stub writes its
// history as a headless child does (meta, the input it reads, a closing
// done when told to finish, a cancelled for a dangling turn on -r) and
// prints the done event serve reacts to. A's orb is played by the
// adapter as A's process would write it (state.json with A's pid, the
// Fake's container), like orb_lifecycle's adapter.
//
// The fields are read off the ground truth: meta.json's queued tasks,
// the agents' histories, the notices in P's history (a report), the
// child table, state.json and the Fake. While serve is up, GetState also
// requires the API to show what the spec derives: A's orb status
// (orb_view) and P's running and queued agent counts.

const sccoStubEnv = "SCCO_STUB"

// The test binary is its own stub child: serve spawns it with
// SCCO_STUB set, and it never reaches TestMain.
func init() {
	if ctl := os.Getenv(sccoStubEnv); ctl != "" {
		os.Exit(sccoStub(ctl))
	}
}

// sccoStub is a headless session child in miniature. It records its pid
// in ctl/<id>.pid, turns every stdin line into an input entry, and
// finishes the open turn when "done" is appended to ctl/<id>.<pid>.cmd.
// It exits on stdin EOF, as the real child does.
func sccoStub(ctl string) int {
	id := os.Getenv("BOUGH_SESSION_ID")
	for i, a := range os.Args {
		if a == "-r" && i+1 < len(os.Args) {
			id = os.Args[i+1]
		}
	}
	path := filepath.Join(os.Getenv("HOME"), ".bough", "history", id+".jsonl")
	if entries, err := history.Read(path); err != nil {
		wd, _ := os.Getwd()
		b, _ := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{
			"cwd": wd, "mode": orDefault(os.Getenv("BOUGH_MODE"), "local"),
			"project": os.Getenv("BOUGH_PROJECT"), "spawned_by": os.Getenv("BOUGH_SPAWNED_BY"),
		}})
		if err := os.WriteFile(path, append(b, '\n'), 0o644); err != nil {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
	} else if turnOpen(entries) {
		// The history mount closes a turn the last process died in.
		history.AppendFile(path, "cancelled", map[string]any{})
	}
	pid := os.Getpid()
	tmp := filepath.Join(ctl, fmt.Sprintf("%s.pid.%d", id, pid))
	os.WriteFile(tmp, []byte(strconv.Itoa(pid)), 0o644)
	os.Rename(tmp, filepath.Join(ctl, id+".pid"))

	go func() {
		sc := bufio.NewScanner(os.Stdin)
		for sc.Scan() {
			if line := sc.Text(); line != "" {
				history.AppendFile(path, "input", map[string]any{"text": line})
			}
		}
		os.Exit(0)
	}()
	cmds := filepath.Join(ctl, fmt.Sprintf("%s.%d.cmd", id, pid))
	done := 0
	for {
		time.Sleep(2 * time.Millisecond)
		b, _ := os.ReadFile(cmds)
		n := strings.Count(string(b), "done\n")
		for ; done < n; done++ {
			history.AppendFile(path, "assistant", map[string]any{"text": "finished"})
			history.AppendFile(path, "done", map[string]any{})
			fmt.Println(`{"kind":"done","text":""}`)
		}
	}
}

func orDefault(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}

const sccoSlug = "proj"

type sccoAdapter struct {
	t    *testing.T
	exe  string
	root string
	gate gate

	// This walk's room.
	n                   int
	home, hist, ctl     string
	rt                  *container.Fake
	sup                 *serve.Supervisor
	api                 *serve.API
	srv                 *httptest.Server
	serve               string // up | closing | down
	p, a, b, c          string
	cur                 string // the project's current image tag
	aMsg                bool
	taken               map[string]int
	resumes, imageBumps int

	// closeAsCrash is TestServeCloseChildrenOrbsCatchesWrongAdapter's
	// bug: Close crashes instead, so no orb is stopped.
	closeAsCrash bool
}

func newSccoAdapter(t *testing.T) *sccoAdapter {
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	// A short root, as orb_lifecycle's: under the system temp dir.
	root, err := os.MkdirTemp("", "scco-")
	if err != nil {
		t.Fatal(err)
	}
	root, _ = filepath.EvalSymlinks(root)
	a := &sccoAdapter{t: t, exe: exe, root: root, taken: map[string]int{}}
	t.Cleanup(func() {
		a.shutdown()
		os.RemoveAll(root)
	})
	return a
}

// shutdown ends whatever serve this walk left, and its children.
func (a *sccoAdapter) shutdown() {
	if a.srv != nil {
		a.srv.Close()
		a.srv = nil
	}
	if a.sup != nil {
		a.sup.Close()
		a.sup = nil
	}
}

// boot is serve starting on this walk's HOME: NewSupervisor (loadMeta,
// requeue, the drain) and the API.
func (a *sccoAdapter) boot() error {
	sup, err := serve.NewSupervisor(serve.Options{
		Exe:      a.exe,
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + a.home, "PATH=" + os.Getenv("PATH"), sccoStubEnv + "=" + a.ctl},
		Runtime:  a.rt,
	})
	if err != nil {
		return err
	}
	a.sup, a.api = sup, serve.NewAPI(sup)
	a.srv = httptest.NewServer(a.api)
	a.serve = "up"
	return nil
}

// Init sets up a fresh room on a fresh HOME: P with one finished turn
// and no process, A running its task with its orb building, B and C
// queued behind the cap of 1.
func (a *sccoAdapter) Init() error {
	a.gate.reset()
	a.shutdown()
	a.n++
	a.home = filepath.Join(a.root, fmt.Sprintf("w%03d", a.n))
	a.hist = filepath.Join(a.home, ".bough", "history")
	a.ctl = filepath.Join(a.home, "ctl")
	for _, d := range []string{a.hist, a.ctl} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}
	a.rt = container.NewFake()
	a.cur = "img-1"
	a.rt.AddImage(a.cur)
	a.aMsg = false
	if err := a.boot(); err != nil {
		return err
	}

	a.p = history.NewID()
	var lines []byte
	for i, e := range []history.Entry{
		{Kind: "meta", Data: map[string]any{"cwd": a.home, "mode": "local"}},
		{Kind: "input", Data: map[string]any{"text": "start the agents"}},
		{Kind: "done", Data: map[string]any{}},
	} {
		e.Seq, e.At = int64(i+1), time.Now()
		b, _ := json.Marshal(e)
		lines = append(append(lines, b...), '\n')
	}
	if err := os.WriteFile(a.histPath(a.p), lines, 0o644); err != nil {
		return err
	}

	var err error
	var queued bool
	if a.a, queued, err = a.sup.CreateChild(serve.CreateOptions{SpawnedBy: a.p, Prompt: "task a", Slug: sccoSlug}, 0, 1); err != nil {
		return err
	} else if queued {
		return fmt.Errorf("A was queued with nothing running")
	}
	if err := a.waitTurns(a.a, 1); err != nil {
		return err
	}
	if err := a.writeOrb(true, func(s *orb.State) { s.Status = orb.StatusBuilding }); err != nil {
		return err
	}
	for _, id := range []*string{&a.b, &a.c} {
		if *id, queued, err = a.sup.CreateChild(serve.CreateOptions{SpawnedBy: a.p, Prompt: "task"}, 0, 1); err != nil {
			return err
		} else if !queued {
			return fmt.Errorf("%s started past a running cap of 1", *id)
		}
	}
	return nil
}

func (a *sccoAdapter) Cleanup() error {
	a.shutdown()
	return nil
}

func (a *sccoAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Room", Index: 0}: a}, nil
}

func (a *sccoAdapter) histPath(id string) string { return filepath.Join(a.hist, id+".jsonl") }

// sccoObs is the room as the ground truth has it.
type sccoObs struct {
	a, b, c         string
	dups            bool
	file, ctr, img  string
	queued, running int
}

func (o sccoObs) alive() bool { return o.a == "r" || o.a == "i" }

// orbView is the spec's orb_view: the status A's row must show.
func (o sccoObs) orbView() string {
	if (o.file == "building" || o.file == "starting" || o.file == "running") && !o.alive() {
		return "stopped"
	}
	return o.file
}

func (a *sccoAdapter) meta() (map[string]serve.SessionMeta, error) {
	b, err := os.ReadFile(filepath.Join(a.home, ".bough", "serve", "meta.json"))
	if err != nil {
		return nil, err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	return f.Sessions, json.Unmarshal(b, &f)
}

// notices counts the reports P's history holds, per agent.
func (a *sccoAdapter) notices() (map[string]int, error) {
	entries, err := history.Read(a.histPath(a.p))
	if err != nil {
		return nil, err
	}
	n := map[string]int{}
	for _, e := range entries {
		if e.Kind == "notice" {
			from, _ := e.Data["from"].(string)
			n[from]++
		}
	}
	return n, nil
}

// pidOf is the pid of the agent's last process, 0 before it had one.
func (a *sccoAdapter) pidOf(id string) int {
	b, err := os.ReadFile(filepath.Join(a.ctl, id+".pid"))
	if err != nil {
		return 0
	}
	pid, _ := strconv.Atoi(string(b))
	return pid
}

func (a *sccoAdapter) live(id string) (bool, error) {
	pid := a.pidOf(id)
	alive := pid > 0 && syscall.Kill(pid, 0) == nil
	if a.serve != "up" {
		// Close and Crash reap every child before they return.
		if alive {
			return false, fmt.Errorf("agent %s's process %d outlived serve", id, pid)
		}
		return false, nil
	}
	return a.sup.Live(id), nil
}

// agent reads one agent's field: q, r, i (A only), xo or t. Anything
// else names what the ground truth shows that the spec has no value
// for, so the comparison fails on it.
func (a *sccoAdapter) agent(id string, m map[string]serve.SessionMeta, notices map[string]int, isA bool) (string, bool, error) {
	if m[id].Task != nil {
		return "q", false, nil
	}
	entries, err := history.Read(a.histPath(id))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return "", false, err
	}
	turns := 0
	for _, e := range entries {
		if e.Kind == "input" {
			turns++
		}
	}
	open := turnOpen(entries)
	live, err := a.live(id)
	if err != nil {
		return "", false, err
	}
	told := notices[id]
	dups := told > turns
	switch {
	case turns == 0:
		return "booting", dups, nil
	case live && open:
		if told != turns-1 {
			return fmt.Sprintf("running with %d of %d earlier turns reported", told, turns-1), dups, nil
		}
		return "r", dups, nil
	case told < turns && open && told == turns-1:
		return "xo", dups, nil
	case told < turns:
		return fmt.Sprintf("%d of %d turns reported (live %v, open %v)", told, turns, live, open), dups, nil
	case live && isA:
		return "i", dups, nil
	}
	return "t", dups, nil
}

func (a *sccoAdapter) observe() (sccoObs, error) {
	var o sccoObs
	m, err := a.meta()
	if err != nil {
		return o, err
	}
	notices, err := a.notices()
	if err != nil {
		return o, err
	}
	for _, x := range []struct {
		id  string
		out *string
	}{{a.a, &o.a}, {a.b, &o.b}, {a.c, &o.c}} {
		v, dups, err := a.agent(x.id, m, notices, x.id == a.a)
		if err != nil {
			return o, err
		}
		*x.out = v
		o.dups = o.dups || dups
		switch v {
		case "q":
			o.queued++
		case "r":
			o.running++
		}
	}
	st, err := orb.ReadState(a.home, a.a)
	if err != nil {
		return o, err
	}
	o.file = string(st.Status)
	if st.Session == "" {
		o.file = "none"
	}
	ctx, cancel := actionCtx()
	defer cancel()
	cs, err := a.rt.Inspect(ctx, container.OrbName(a.a))
	if err != nil {
		return o, err
	}
	o.ctr, o.img = string(cs), "cur"
	if cs != container.StateMissing {
		imgs, _ := a.rt.ContainerImages(ctx)
		if len(imgs) != 1 || imgs[0] != a.cur {
			o.img = "old"
		}
	}
	return o, nil
}

func (a *sccoAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	if a.serve == "up" {
		if err := a.checkViews(o); err != nil {
			return nil, err
		}
	}
	return map[string]any{
		"serve": a.serve, "a": o.a, "b": o.b, "c": o.c, "a_msg": a.aMsg, "dups": o.dups,
		"file": o.file, "ctr": o.ctr, "img": o.img,
	}, nil
}

// checkViews requires the API to show A's orb as the spec's orb_view
// and P's agents as the room counts them.
func (a *sccoAdapter) checkViews(o sccoObs) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var bad []string
	var ra struct {
		Session serve.Row `json:"session"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.a, &ra); err != nil {
		return err
	}
	if ra.Session.Orb == nil {
		bad = append(bad, "A's row has no orb")
	} else if got := string(ra.Session.Orb.Status); got != o.orbView() {
		bad = append(bad, fmt.Sprintf("A's orb shows %s, the model says %s", got, o.orbView()))
	}
	var rp struct {
		Session serve.Row `json:"session"`
	}
	if err := a.get(ctx, "/api/sessions/"+a.p, &rp); err != nil {
		return err
	}
	if ag := rp.Session.Agents; ag == nil || ag.Running != o.running || ag.Queued != o.queued {
		bad = append(bad, fmt.Sprintf("P's agents are %+v, the model says running %d, queued %d", ag, o.running, o.queued))
	}
	if len(bad) > 0 {
		return fmt.Errorf("serve's view disagrees with the model in %+v: %s", o, strings.Join(bad, "; "))
	}
	return nil
}

func (a *sccoAdapter) get(ctx context.Context, path string, out any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.srv.URL+path, nil)
	if err != nil {
		return err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s = %d", path, resp.StatusCode)
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// --- waits ---

// poll checks ok every few ms until it holds or limit passes.
func sccoPoll(what string, limit time.Duration, ok func() (bool, error)) error {
	deadline := time.Now().Add(limit)
	for {
		done, err := ok()
		if err != nil {
			return err
		}
		if done {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: timed out after %s", what, limit)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func (a *sccoAdapter) waitTurns(id string, n int) error {
	return sccoPoll(fmt.Sprintf("%s to take turn %d", id, n), actionTimeout, func() (bool, error) {
		entries, _ := history.Read(a.histPath(id))
		turns := 0
		for _, e := range entries {
			if e.Kind == "input" {
				turns++
			}
		}
		return turns >= n && turnOpen(entries), nil
	})
}

// settle waits until serve has nothing left to do on its own: no agent
// booting, no report owed while serve is up, and no queued agent while
// the cap has room; then the room must hold still for a moment, or a
// drain past the cap would be missed.
func (a *sccoAdapter) settle() error {
	quiet := func() (sccoObs, bool, error) {
		o, err := a.observe()
		if err != nil {
			return o, false, err
		}
		for _, v := range []string{o.a, o.b, o.c} {
			switch v {
			case "q", "r", "i", "t":
			case "xo":
				if a.serve == "up" {
					return o, false, nil
				}
			default:
				return o, false, nil
			}
		}
		return o, o.queued == 0 || o.running > 0, nil
	}
	var last sccoObs
	err := sccoPoll("serve to settle", 5*time.Second, func() (bool, error) {
		o, ok, err := quiet()
		last = o
		return ok, err
	})
	if err != nil {
		return fmt.Errorf("%w; the room: %+v", err, last)
	}
	time.Sleep(100 * time.Millisecond)
	return nil
}

// --- the orb, as A's process writes it ---

func (a *sccoAdapter) writeOrb(fresh bool, edit func(*orb.State)) error {
	st, err := orb.ReadState(a.home, a.a)
	if err != nil {
		return err
	}
	if fresh {
		st = orb.State{Session: a.a, Project: sccoSlug, Image: a.cur, Container: container.OrbName(a.a)}
	}
	edit(&st)
	st.PID = a.pidOf(a.a)
	st.UpdatedAt = time.Now().UTC()
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	return writeFileAtomic(filepath.Join(orb.Dir(a.home, a.a), "state.json"), b)
}

// OrbUp is orb.Start past EnsureImage: a container on a replaced image
// is removed and created again, then it runs and resume.sh starts.
func (a *sccoAdapter) OrbUp() error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(o.alive() && o.file == "building") {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	name := container.OrbName(a.a)
	if o.ctr != "missing" && o.img == "old" {
		if err := a.rt.Remove(ctx, name); err != nil {
			return err
		}
	}
	if err := a.rt.Start(ctx, container.RunSpec{Name: name, Image: a.cur}); err != nil {
		return err
	}
	return a.writeOrb(false, func(s *orb.State) { s.Status, s.Phase, s.Image = orb.StatusStarting, orb.PhaseResume, a.cur })
}

func (a *sccoAdapter) OrbReady() error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(o.alive() && o.file == "starting") {
		return err
	}
	return a.writeOrb(false, func(s *orb.State) { s.Status, s.Phase = orb.StatusRunning, orb.PhaseReady })
}

// NewImage is the project rebuilt: a new tag is current.
func (a *sccoAdapter) NewImage() error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(o.ctr != "missing" && o.img == "cur" && !o.alive()) {
		return err
	}
	a.imageBumps++
	a.cur = fmt.Sprintf("img-%d", a.imageBumps+1)
	a.rt.AddImage(a.cur)
	return nil
}

// --- the agents' turns ---

// finish tells an agent's process to close its turn, and waits for the
// report and whatever the freed slot starts.
func (a *sccoAdapter) finish(id string, enabled func(sccoObs) bool) error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(a.serve == "up" && enabled(o)) {
		return err
	}
	cmds := filepath.Join(a.ctl, fmt.Sprintf("%s.%d.cmd", id, a.pidOf(id)))
	f, err := os.OpenFile(cmds, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	_, err = f.WriteString("done\n")
	f.Close()
	if err != nil {
		return err
	}
	err = sccoPoll(id+"'s report", actionTimeout, func() (bool, error) {
		entries, _ := history.Read(a.histPath(id))
		n, err := a.notices()
		turns := 0
		for _, e := range entries {
			if e.Kind == "input" {
				turns++
			}
		}
		return !turnOpen(entries) && n[id] >= turns, err
	})
	if err != nil {
		return err
	}
	return a.settle()
}

func (a *sccoAdapter) FinishA() error {
	return a.finish(a.a, func(o sccoObs) bool { return o.a == "r" && o.file == "running" })
}

func (a *sccoAdapter) FinishB() error {
	return a.finish(a.b, func(o sccoObs) bool { return o.b == "r" })
}

func (a *sccoAdapter) FinishC() error {
	return a.finish(a.c, func(o sccoObs) bool { return o.c == "r" })
}

// ResumeA is a person messaging A: Send starts `-r` and writes the
// prompt, and the new process prepares its orb again.
func (a *sccoAdapter) ResumeA() error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(a.serve == "up" && o.a == "t") {
		return err
	}
	entries, _ := history.Read(a.histPath(a.a))
	turns := 0
	for _, e := range entries {
		if e.Kind == "input" {
			turns++
		}
	}
	a.resumes++
	if err := a.sup.Send(a.a, fmt.Sprintf("message %d", a.resumes)); err != nil {
		return err
	}
	if err := a.waitTurns(a.a, turns+1); err != nil {
		return err
	}
	a.aMsg = true
	return a.writeOrb(false, func(s *orb.State) { s.Status, s.Phase, s.Portals = orb.StatusBuilding, orb.PhaseBuild, nil })
}

// --- serve going down and up ---

// Close is SIGTERM: serveForeground's srv.Shutdown, then sup.Close.
func (a *sccoAdapter) Close() error {
	if !a.gate.pass(a.serve == "up") {
		return nil
	}
	a.srv.Close()
	a.srv = nil
	if a.closeAsCrash {
		a.sup.Crash()
	} else if err := a.sup.Close(); err != nil {
		return err
	}
	a.serve = "closing"
	return nil
}

// Exit is the process ending after Close returned.
func (a *sccoAdapter) Exit() error {
	if !a.gate.pass(a.serve == "closing") {
		return nil
	}
	a.sup, a.api, a.serve = nil, nil, "down"
	return nil
}

func (a *sccoAdapter) Crash() error {
	if !a.gate.pass(a.serve == "up") {
		return nil
	}
	a.srv.Close()
	a.srv = nil
	a.sup.Crash()
	a.sup, a.api, a.serve = nil, nil, "down"
	return nil
}

// Boot is launchd starting serve again on the same HOME.
func (a *sccoAdapter) Boot() error {
	if !a.gate.pass(a.serve == "down") {
		return nil
	}
	if err := a.boot(); err != nil {
		return err
	}
	return a.settle()
}

// Reap is a reaper tick long after the last activity.
func (a *sccoAdapter) Reap() error {
	o, err := a.observe()
	if err != nil || !a.gate.pass(a.serve == "up" && o.ctr == "running" && !o.alive()) {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.api.ReapIdleOrbs(ctx, time.Hour, time.Now().Add(100*time.Hour))
	return nil
}

// sccoAction counts an action that ran: the gate was open before it and
// still is after it.
func sccoAction(name string, f func(*sccoAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*sccoAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
		}
		return nil, err
	}
}

var sccoActions = map[string]map[string]fmbt.ActionFunc{"Room": {
	"OrbUp":    sccoAction("OrbUp", (*sccoAdapter).OrbUp),
	"OrbReady": sccoAction("OrbReady", (*sccoAdapter).OrbReady),
	"NewImage": sccoAction("NewImage", (*sccoAdapter).NewImage),
	"FinishA":  sccoAction("FinishA", (*sccoAdapter).FinishA),
	"FinishB":  sccoAction("FinishB", (*sccoAdapter).FinishB),
	"FinishC":  sccoAction("FinishC", (*sccoAdapter).FinishC),
	"ResumeA":  sccoAction("ResumeA", (*sccoAdapter).ResumeA),
	"Close":    sccoAction("Close", (*sccoAdapter).Close),
	"Exit":     sccoAction("Exit", (*sccoAdapter).Exit),
	"Crash":    sccoAction("Crash", (*sccoAdapter).Crash),
	"Boot":     sccoAction("Boot", (*sccoAdapter).Boot),
	"Reap":     sccoAction("Reap", (*sccoAdapter).Reap),
}}

func sccoOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 10, "max-parallel-runs": 0}
}

// sccoWalk drives the adapter down every walk and compares the room with
// the spec's state at each step. It returns the first difference, and
// A's transcript from every walk.
func sccoWalk(a *sccoAdapter, walks []tracecheck.Walk) ([][]history.Entry, error) {
	var transcripts [][]history.Entry
	for i, w := range walks {
		for j, step := range w.Trace {
			var err error
			if j == 0 {
				err = a.Init()
			} else {
				name := strings.TrimPrefix(step.Action, "Room#0.")
				f, ok := sccoActions["Room"][name]
				if !ok {
					return nil, fmt.Errorf("walk %d step %d: no action %s", i, j, step.Action)
				}
				_, err = f(a, nil)
				if err == nil && a.gate.off {
					err = errors.New("the adapter found it disabled")
				}
			}
			if err != nil {
				return nil, fmt.Errorf("walk %d step %d (%s): %w", i, j, step.Action, err)
			}
			got, err := a.GetState()
			if err != nil {
				return nil, fmt.Errorf("walk %d step %d (%s): state: %w", i, j, step.Action, err)
			}
			if d := roomDiff(step.State, got); d != "" {
				return nil, fmt.Errorf("walk %d step %d (%s): %s", i, j, step.Action, d)
			}
		}
		entries, err := history.Read(a.histPath(a.a))
		if err != nil {
			return nil, err
		}
		transcripts = append(transcripts, entries)
		a.Cleanup()
	}
	return transcripts, nil
}

func sccoWalks(t *testing.T, cover tracecheck.Cover) []tracecheck.Walk {
	t.Helper()
	b, err := pathsJSONCover("serve_close_children_orbs", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Paths) == 0 {
		t.Fatal("no walks")
	}
	return f.Paths
}

// serveCloseChildrenOrbsHistory reads A's transcript as the spec's steps
// on A alone. A's file records only its own turns, so the steps it
// implies are filled in the one way the spec allows them: a closed turn
// needed its orb up (OrbUp, OrbReady, FinishA); a new input after a
// finished turn needed A's process gone (a Crash kills it idle) and
// serve back (Boot); a dangling turn closed as cancelled is a cut turn
// (Crash), reported at Boot, and the input after it is ResumeA, which
// prepares the orb again.
func serveCloseChildrenOrbsHistory(entries []history.Entry) []tracecheck.Step {
	st := func(a string) map[string]any { return map[string]any{"Room#0.a": a} }
	step := func(act, a string) tracecheck.Step { return tracecheck.Step{Action: "Room#0." + act, State: st(a)} }
	var steps []tracecheck.Step
	open, cut, lastClose := false, false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if len(steps) == 0 {
				steps = append(steps, tracecheck.Step{Action: "Init", State: st("r")})
			} else {
				if !cut && !open {
					steps = append(steps, step("Crash", "t"))
				}
				if !open {
					steps = append(steps, step("Boot", "t"))
				}
				steps = append(steps, step("ResumeA", "r"))
			}
			open, cut, lastClose = true, false, ""
		case "cancelled":
			if open {
				steps = append(steps, step("Crash", "xo"))
				cut = true
			}
			open, lastClose = false, "cancelled"
		case "done":
			if !open || lastClose == "cancelled" {
				continue
			}
			steps = append(steps, step("OrbUp", "r"), step("OrbReady", "r"), step("FinishA", "i"))
			open, lastClose = false, "done"
		}
	}
	return steps
}

func init() { historyProjections["serve_close_children_orbs"] = serveCloseChildrenOrbsHistory }

// Every walk of the checked-in graph (every state; every transition
// under MODEL_COVER=transitions) against the in-process serve, then A's
// transcript from each walk replayed on the graph.
func TestServeCloseChildrenOrbsPaths(t *testing.T) {
	t.Parallel()
	a := newSccoAdapter(t)
	walks := sccoWalks(t, envCover())
	transcripts, err := sccoWalk(a, walks)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%d walks, actions taken: %v", len(walks), a.taken)
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "serve_close_children_orbs"))
	if err != nil {
		t.Fatal(err)
	}
	for _, entries := range transcripts {
		checkHistory(t, g, entries, serveCloseChildrenOrbsHistory)
	}
}

// The runner's random walks over the same adapter: the exhaustive run's.
func TestServeCloseChildrenOrbs(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSccoAdapter(t)
	if err := runMBT(t, "serve_close_children_orbs", a, sccoActions, sccoOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
}

// A Close that leaves the killed child's orb up must fail the walks, or
// a green run proves nothing. It shows on one transition (Close with A's
// container running), so the walks take every transition.
func TestServeCloseChildrenOrbsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSccoAdapter(t)
	a.closeAsCrash = true
	_, err := sccoWalk(a, sccoWalks(t, tracecheck.CoverTransitions))
	if err == nil {
		t.Fatal("walks whose Close leaves the orb running passed; the walk is not checking state")
	}
	t.Logf("caught, as it must be: %v", err)
}

// The projection reads a resume after a cut, and a transcript that lost
// the cut's "cancelled" (a second input into an open turn) is not a
// path in the spec.
func TestServeCloseChildrenOrbsHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "serve_close_children_orbs"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	ok := [][]history.Entry{
		{e("meta"), e("input"), e("cancelled"), e("input"), e("assistant"), e("done")},
		{e("meta"), e("input"), e("assistant"), e("done"), e("input")},
		{e("meta"), e("input"), e("done"), e("cancelled"), e("input"), e("cancelled"), e("done"), e("input")},
	}
	for i, tr := range ok {
		if v := g.Check(serveCloseChildrenOrbsHistory(tr)); v != nil {
			t.Errorf("transcript %d: %v", i, v)
		}
	}
	lost := []history.Entry{e("meta"), e("input"), e("input"), e("done")}
	if v := g.Check(serveCloseChildrenOrbsHistory(lost)); v == nil {
		t.Fatal("a second input into an open turn passed the trace check")
	}
}
