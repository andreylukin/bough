package orb

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/tools"
)

// restarter applies a project's current definition to this running
// session's orb: /orb restart, `bough project restart` (from a shell, or
// relayed from the guest by the agent's own bash) and serve's POST
// /api/sessions/{id}/orb/restart all end here. The last two cannot reach
// this process, so they write restart.json and watch picks it up.
//
// The order is the point. The new image builds while the old container
// keeps serving the session; a build that fails changes nothing but a
// notice. The swap waits until no turn is open, because the call that
// asked is usually the agent's bash running IN the old container, and
// swapping under it would kill the request that asked for the swap. The
// result reaches the agent as a job notice (which wakes an idle one),
// and the changed facts (address, ports) as the orb prompt section,
// which the engine sends as a <context-update>: the frozen system prompt
// is never edited.
type restarter struct {
	h    *handle
	kctx *kernel.Context // job-notices, looked up when needed, never during Apply
	// poll is how often restart.json is looked for: a stat a second is
	// free (the engine's stored-notice poll does the same).
	poll time.Duration
	// settle is how long no turn must have run before a swap: a wake turn
	// can start right after "done".
	settle time.Duration
	// prep is the host half of a start for an orb that never opened
	// (prepare; tests swap it for one that does not chdir).
	prep func(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratch string) (*iorb.Orb, error)
	load func() (projectdef.Project, error)

	mu        sync.Mutex
	busy      bool      // a turn is open
	lastEvent time.Time // the last turn event, busy or done
	pending   *iorb.RestartRequest
	gen       int // bumps on each request: a build for an older one is dropped
	notify    func(string)
	created   time.Time

	kick   chan struct{}
	ctx    context.Context
	cancel context.CancelFunc
	wg     sync.WaitGroup
}

func newRestarter(h *handle, kctx *kernel.Context) *restarter {
	ctx, cancel := context.WithCancel(context.Background())
	r := &restarter{
		h: h, kctx: kctx, poll: time.Second, settle: 300 * time.Millisecond,
		prep:    prepare,
		load:    func() (projectdef.Project, error) { return projectdef.Load(h.home, h.slug) },
		notify:  func(s string) { fmt.Fprintf(os.Stderr, "bough: %s\n", s) },
		created: time.Now(),
		kick:    make(chan struct{}, 1),
		ctx:     ctx, cancel: cancel,
	}
	return r
}

// start runs the swap loop and the request-file watcher.
func (r *restarter) start() {
	r.wg.Add(2)
	go func() { defer r.wg.Done(); r.run() }()
	go func() { defer r.wg.Done(); r.watch() }()
}

// stop ends both goroutines, a build in flight included, and waits.
func (r *restarter) stop() {
	r.cancel()
	r.wg.Wait()
}

func (r *restarter) setNotify(f func(string)) {
	r.mu.Lock()
	r.notify = f
	r.mu.Unlock()
}

func (r *restarter) say(text string) {
	r.mu.Lock()
	f := r.notify
	r.mu.Unlock()
	f(text)
}

// jobs are the session's running background jobs, which die with the
// container they run in.
func (r *restarter) jobs() []tools.Running {
	if r.kctx == nil {
		return nil
	}
	if j, err := kernel.Get[interface{ Running() []tools.Running }](r.kctx, "job-notices"); err == nil {
		return j.Running()
	}
	return nil
}

// turnKinds are loop events only a running turn emits. A positive list:
// title, todo and activity events arrive after "done" too, and treating
// them as a turn would hold a restart until the next one ended. A
// background subagent's events (sub:*) are not this session's turn; its
// in-flight calls stop with the container like any background job.
var turnKinds = map[string]bool{
	"assistant": true, "assistant-delta": true, "thinking": true, "thinking-delta": true,
	"code": true, "call": true, "result": true, "steer": true,
}

// observe tracks whether a turn is open from the loop's events. Both
// engines end every turn, a cancelled one included, with "done".
func (r *restarter) observe(kind string) {
	busy, done := turnKinds[kind], kind == "done"
	if !busy && !done {
		return
	}
	r.mu.Lock()
	r.busy = busy
	r.lastEvent = time.Now()
	r.mu.Unlock()
}

// request queues a restart and says what will happen. It never blocks
// and never refuses: the build starts now, the swap waits for the turn.
func (r *restarter) request(req iorb.RestartRequest) string {
	r.mu.Lock()
	if r.pending == nil {
		cp := req
		r.pending = &cp
	} else {
		r.pending.Fresh = r.pending.Fresh || req.Fresh
		r.pending.By = req.By
	}
	r.gen++
	busy := r.busy
	r.mu.Unlock()
	if o := r.h.Orb(); o != nil {
		o.SetRestart(iorb.RestartPending)
	}
	select {
	case r.kick <- struct{}{}:
	default:
	}
	if r.h.Orb() == nil && !r.h.Starting() {
		return fmt.Sprintf("Restart of orb %s scheduled: this session has no orb, so it starts from the project's current definition now; a notice reports the result.", r.h.slug)
	}
	when := "as soon as it is ready"
	if busy {
		when = "when the current turn ends"
	}
	return fmt.Sprintf("Restart of orb %s scheduled: the image builds from the project's current definition now, while this container keeps running, and the container is swapped %s. Background jobs stop at the swap; a notice reports the result.", r.h.slug, when)
}

// watch picks up restart.json written by another process. A request from
// before this session's orb opened is dropped: that start already applied
// the definition it asked for.
func (r *restarter) watch() {
	t := time.NewTicker(r.poll)
	defer t.Stop()
	for {
		select {
		case <-r.ctx.Done():
			return
		case <-t.C:
		}
		if req, ok := iorb.TakeRestart(r.h.home, r.h.session); ok && !req.At.Before(r.created) {
			r.request(req)
		}
	}
}

func (r *restarter) run() {
	for {
		select {
		case <-r.ctx.Done():
			return
		case <-r.kick:
		}
		// A request made while the first start runs waits for it.
		select {
		case <-r.h.ready:
		case <-r.ctx.Done():
			return
		}
		r.mu.Lock()
		req, g := r.pending, r.gen
		r.pending = nil
		r.mu.Unlock()
		if req != nil {
			r.once(*req, g)
		}
	}
}

// stale reports a newer request since gen g; its Fresh is kept.
func (r *restarter) stale(g int, fresh bool) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.gen == g {
		return false
	}
	if r.pending != nil {
		r.pending.Fresh = r.pending.Fresh || fresh
	}
	return true
}

// once is one restart: build, wait for the turn, swap, report.
func (r *restarter) once(req iorb.RestartRequest, g int) {
	h := r.h
	old := h.Orb()
	p, err := r.load()
	if err != nil {
		if old != nil {
			old.SetRestart("")
		}
		r.say(failureNotice(h.slug, iorb.PhaseStart, fmt.Errorf("orb: restart %s: %w", h.session, err), old != nil))
		return
	}
	bctx, cancel := context.WithTimeout(r.ctx, 30*time.Minute)
	defer cancel()
	if old == nil {
		// The first start failed: there is no container to protect, so
		// this is a start like the first, now.
		o, err := r.prep(bctx, h.rt, h.home, h.session, p, h.scratch)
		if err == nil {
			err = o.Start(bctx)
		}
		if r.ctx.Err() != nil {
			return
		}
		if err != nil {
			h.failed(err)
			h.doRefresh()
			st, _ := iorb.ReadState(h.home, h.session)
			r.say(failureNotice(h.slug, iorb.FailedAt(st), err, false))
			return
		}
		h.beginSwap()(o)
		h.doRefresh()
		st := o.State()
		r.say(noticeText(h.slug, iorb.Replaced{Image: st.Image, Recreated: true, State: st}, nil, iorb.ResumeTail(h.home, h.session, 20)))
		return
	}
	old.SetRestart(iorb.RestartBuilding)
	next, err := old.Successor(bctx, p)
	if r.stale(g, req.Fresh) || r.ctx.Err() != nil {
		return // a newer request rebuilds; nothing was created
	}
	if err != nil {
		old.SetRestart("")
		phase := iorb.PhaseStart
		if strings.Contains(err.Error(), "orb: image") {
			phase = iorb.PhaseBuild
		}
		r.say(failureNotice(h.slug, phase, err, true))
		return
	}
	old.SetRestart(iorb.RestartPending)
	if err := r.waitIdle(r.ctx); err != nil {
		return
	}
	if r.stale(g, req.Fresh) {
		return
	}
	stopped := r.jobs()
	end := h.beginSwap()
	sctx, scancel := context.WithTimeout(context.Background(), 5*time.Minute)
	res, err := old.Replace(sctx, next, req.Fresh)
	scancel()
	if err != nil {
		end(nil)
		old.SetRestart("")
		h.doRefresh()
		r.say(failureNotice(h.slug, iorb.PhaseStart, err, true))
		return
	}
	end(next)
	h.doRefresh()
	r.say(noticeText(h.slug, res, stopped, iorb.ResumeTail(h.home, h.session, 20)))
}

// waitIdle returns once no turn has been open for settle.
func (r *restarter) waitIdle(ctx context.Context) error {
	tick := min(max(r.settle/4, 5*time.Millisecond), 100*time.Millisecond)
	for {
		r.mu.Lock()
		ok := !r.busy && time.Since(r.lastEvent) >= r.settle
		r.mu.Unlock()
		if ok {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(tick):
		}
	}
}

// noticeText is what the agent (and the person) read after a swap: the
// image, whether the container was recreated and why, the jobs that died
// with it, and this start's resume.log.
func noticeText(slug string, res iorb.Replaced, stopped []tools.Running, resumeTail string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "orb restarted for project %s: image %s", slug, res.Image)
	switch {
	case res.PrevImage == "":
	case res.PrevImage != res.Image:
		fmt.Fprintf(&b, " (was %s)", res.PrevImage)
	default:
		b.WriteString(" (unchanged)")
	}
	switch {
	case res.Recreated && len(res.Why) > 0:
		fmt.Fprintf(&b, "; container recreated (%s)", strings.Join(res.Why, ", "))
	case res.Recreated:
		b.WriteString("; container recreated")
	default:
		b.WriteString("; container restarted in place")
	}
	b.WriteString(".")
	if len(stopped) > 0 {
		names := make([]string, len(stopped))
		for i, j := range stopped {
			names[i] = j.Cmd
		}
		fmt.Fprintf(&b, " %d background job(s) stopped with the old container: %s — start them again if still needed.", len(stopped), strings.Join(names, ", "))
	}
	if res.State.IP != "" {
		fmt.Fprintf(&b, " Address: %s.", res.State.IP)
	}
	if len(res.State.Portals) > 0 {
		b.WriteString(" Portals kept at the same URLs.")
	}
	if res.State.Status == iorb.StatusFailed && res.State.Error != "" {
		fmt.Fprintf(&b, " Setup failed: %s.", res.State.Error)
	} else {
		b.WriteString(" resume.sh ok.")
	}
	if resumeTail != "" {
		b.WriteString("\n\nresume.log:\n" + resumeTail)
	}
	return b.String()
}

// failureNotice is a restart that did not happen: why, the fix, and
// that the session keeps the orb it had.
func failureNotice(slug, phase string, err error, kept bool) string {
	text := strings.Replace(failureReport(slug, phase, err), "orb for project", "orb restart for project", 1)
	if kept {
		return text + "\nThe old orb keeps running; nothing was swapped."
	}
	return text + "\nThis session still has no orb."
}
