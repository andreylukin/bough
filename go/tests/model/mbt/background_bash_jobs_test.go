//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"math/rand/v2"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/background_bash_jobs.fizz against a real serve: one session on
// engine-unreal, the model (llm-control) starting a background bash job
// with or without an until pattern, the job's notices waking an idle
// agent or queueing at the request boundary of a turn in flight, and
// Stop, the Work dialog's kill and the step budget cutting in.
//
// The job is a shell loop the adapter steers through files in its own
// dir: `match` makes it print the until pattern once, `exit` makes it
// exit 0. A turn that is open always has a held llm-control request in
// flight, so a notice that lands in it is queued, as the spec's
// NoticeDelivered says. What reached the model is read off the requests
// llm-control took (control.Request): a notice sent at a boundary is
// recorded nowhere else.
//
// The spec splits steps the real system takes in one breath; the
// adapter keeps its own record (bbjShadow, the spec's role field for
// field) and reads the server wherever the real system is at the
// spec's step:
//
//   - A job event (OutputMatchesUntil, JobExits, the Work dialog's
//     KillJob) is a notice the watch goroutine delivers at once, so the
//     adapter makes it real at the spec's NoticeDelivered instead
//     (`deferred`). Until then job and typed are the adapter's record;
//     pending and waits are internal to the child and always are.
//   - The model's JobKill is a tool call, so it is real at once. The
//     job's shell leaves a helper holding its stdout for a second after
//     a kill (`slow`), so the notice lands after the next request is in
//     flight: queued, as the spec's next NoticeDelivered expects
//     (`landing`).
//   - Stop is SIGINT: the turn is cancelled and the child exits, killing
//     the job, all at once. The adapter waits for that outcome at Stop
//     and reports the spec's steps in between from its record until
//     ChildExits (`ahead`). The step budget is the same up to
//     CancelCloses: BudgetStop makes the model run padding calls until
//     the gate refuses a request (max_steps).
//   - LimitExpires is a timer, so it is made real as a kill (`limited`).
//
// Where a walk asks for an interleaving the real system cannot be
// steered into, the walk follows the spec only from there on
// (`detached`) and is counted in the log: two notices delivered
// together (the watch goroutine delivers the first as soon as it is
// queued), a notice pending at Stop or a job event after it (the exit
// takes milliseconds, and puts the notice nowhere but the quit log), a
// step between JobKill and its notice, and a budget stop with notices
// queued (the gate refuses only at a request's start, after the
// boundary sent them). The log also counts the spec's settled states
// the walks compared against the server.

// bbjMaxSteps is the engine's step budget in these serves: BudgetStop
// pads a turn up to it. It sits above any turn a walk takes on its own.
const bbjMaxSteps = 24

const bbjConfig = controlConfig + "- id: loop\n  plugin: engine-unreal\n  config:\n    max_steps: 24\n"

// bbjMatch is the job's until pattern and what it prints on `match`.
const bbjMatch = "BBJ-MATCH"

// bbjShadow is the spec's Session role, field for field.
type bbjShadow struct {
	child, turn, job, typed, last         string
	until, waits                          bool
	pending, queued, model, lost, quitLog []string
	wakes, prompts, stops, budget         int
}

func newBBJShadow() bbjShadow {
	return bbjShadow{child: "alive", turn: "none", job: "none", typed: "none"}
}

func (s bbjShadow) state() map[string]any {
	l := func(x []string) []string { return append([]string{}, x...) }
	return map[string]any{
		"child": s.child, "turn": s.turn, "job": s.job, "until": s.until, "typed": s.typed,
		"pending": l(s.pending), "queued": l(s.queued), "waits": s.waits, "model": l(s.model),
		"lost": l(s.lost), "quit_log": l(s.quitLog), "wakes": s.wakes, "last": s.last,
		"prompts": s.prompts, "stops": s.stops, "budget": s.budget,
	}
}

func (s *bbjShadow) open() bool { return s.turn == "user" || s.turn == "wake" }

// matchSeen is the spec's `"match" in pending + queued + model + lost + quit_log`.
func (s *bbjShadow) matchSeen() bool {
	for _, l := range [][]string{s.pending, s.queued, s.model, s.lost, s.quitLog} {
		for _, x := range l {
			if x == "match" {
				return true
			}
		}
	}
	return false
}

// flush is a request boundary: the queued notices go to the model.
func (s *bbjShadow) flush() {
	s.model = append(s.model, s.queued...)
	s.queued = nil
}

func (s *bbjShadow) finish(job string) {
	s.job, s.typed = job, "finished"
	s.pending = append(s.pending, "finish")
}

type bbjAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate
	sh   bbjShadow
	// want is the path's state after the step being driven, when a path
	// is walked: it picks BashBackground's until. nil: pick at random.
	want map[string]any

	walk   int
	id     string
	ids    []string
	jobDir string
	seq    int
	held   string   // the llm-control turn in flight, "" when none
	takes  int      // requests the open turn has made (the step budget's count)
	names  []string // the turns this walk's session took, in order

	deferred []string // job events the spec has made and the adapter has not (see above)
	landing  bool     // a JobKill's notice is on its way
	limited  bool     // LimitExpires was made real as a kill (see LimitExpires)
	ahead    string   // "stop" | "budget": the server is past the spec's step

	detached   bool
	why        string
	detachedBy map[string]int
	depth      int
	depths     map[int]int
	checked    map[string]bool // spec states compared against the server, not the record

	// exitAsKill is TestBackgroundBashJobsCatchesWrongAdapter's bug: the
	// job's own exit is made real as the Work dialog's kill.
	exitAsKill bool
}

func newBBJAdapter(t *testing.T) *bbjAdapter {
	s := servetest.Start(t, servetest.Options{Config: bbjConfig})
	return &bbjAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *bbjAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.walk++
	a.jobDir = filepath.Join(a.s.Root, fmt.Sprintf("job%04d", a.walk))
	if err := os.MkdirAll(a.jobDir, 0o755); err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.sh = newBBJShadow()
	a.held, a.takes, a.names = "", 0, nil
	a.deferred, a.landing, a.limited, a.ahead = nil, false, false, ""
	a.detached, a.why = false, ""
	a.gate.reset()
	return nil
}

// Cleanup archives the walk's session, which ends its child and so its
// job, and empties the llm queue: a turn this walk queued and never
// took would otherwise answer the next walk's first request.
func (a *bbjAdapter) Cleanup() error {
	if a.detached {
		if a.detachedBy == nil {
			a.detachedBy = map[string]int{}
		}
		a.detachedBy[a.why]++
	}
	if a.depths == nil {
		a.depths = map[int]int{}
	}
	a.depths[a.depth]++
	a.depth = 0
	ctx, cancel := actionCtx()
	defer cancel()
	// A session stopped before it wrote anything is forgotten with its
	// child: nothing is left to archive.
	var e *servetest.APIError
	if _, err := a.s.Archive(ctx, a.id); errors.As(err, &e) && e.Status == http.StatusNotFound {
		return nil
	} else if err != nil {
		return err
	}
	if _, err := a.wait("the archived child to exit", actionTimeout, func(r serve.Row, _ []serve.Line) bool { return !r.Live }); err != nil {
		return err
	}
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		if n, ok := strings.CutSuffix(e.Name(), ".json"); ok {
			os.Remove(filepath.Join(a.dir, n+".json"))
		}
	}
	a.held = ""
	return nil
}

func (a *bbjAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// GetState is the adapter's record, overlaid with what the server shows
// wherever the real system is at the spec's step.
func (a *bbjAdapter) GetState() (map[string]any, error) {
	st := a.sh.state()
	if a.detached || a.ahead == "stop" {
		return st, nil
	}
	row, lines, err := a.read()
	if err != nil {
		return nil, err
	}
	r := bbjObserve(row, lines, a.requests())
	if a.ahead == "" {
		st["child"], st["turn"], st["last"] = r.child, r.turn, r.last
	}
	if len(a.deferred) == 0 && !a.landing {
		st["typed"] = r.typed
		if !a.limited {
			st["job"] = r.job
		}
	}
	st["until"], st["wakes"] = r.until, r.wakes
	st["queued"], st["model"], st["lost"], st["quit_log"] = r.queued, r.model, r.lost, r.quitLog
	return st, nil
}

// requests is every user message the walk's requests carried.
func (a *bbjAdapter) requests() []string {
	var all []string
	for _, n := range a.names {
		msgs, _ := control.Request(a.dir, n)
		all = append(all, msgs...)
	}
	return all
}

// bbjReal is what the server shows of the spec's fields.
type bbjReal struct {
	child, turn, job, typed, last string
	until                         bool
	queued, model, lost, quitLog  []string
	wakes                         int
}

var (
	bbjMatchRe  = regexp.MustCompile(`job \d+ matched "`)
	bbjFinishRe = regexp.MustCompile(`(?m)^job \d+ \[[^\]]*\].*$`)
)

// bbjItems is the notices a text carries, in order: "match" for an
// until match, "finish" for a job's finish line.
func bbjItems(text string) []string {
	type at struct {
		i    int
		item string
	}
	var got []at
	for _, m := range bbjMatchRe.FindAllStringIndex(text, -1) {
		got = append(got, at{m[0], "match"})
	}
	for _, m := range bbjFinishRe.FindAllStringIndex(text, -1) {
		got = append(got, at{m[0], "finish"})
	}
	sort.Slice(got, func(i, j int) bool { return got[i].i < got[j].i })
	out := []string{}
	for _, g := range got {
		out = append(out, g.item)
	}
	return out
}

// bbjFinal is how a finished job ended, from its finish notice's line.
func bbjFinal(text string) string {
	line := bbjFinishRe.FindString(text)
	switch {
	case line == "":
		return ""
	case strings.Contains(line, "[exited 0]"):
		return "exited"
	case strings.Contains(line, "killed when bough quit"):
		return "quit"
	case strings.Contains(line, "killed after"):
		return "limit"
	default:
		return "killed"
	}
}

// bbjObserve reads the spec's fields off a transcript and the requests
// the model was sent. A text-only "job" entry written in an open turn
// is a queued notice; it reached the model when a later request carried
// it as a "[notice]" input, and is lost when the turn closed without
// that. One written with no turn open is stopJobs' at exit.
func bbjObserve(row serve.Row, lines []serve.Line, requests []string) bbjReal {
	r := bbjReal{child: "gone", turn: "none", job: "none", typed: "none",
		queued: []string{}, model: []string{}, lost: []string{}, quitLog: []string{}}
	if row.Live {
		r.child = "alive"
	}
	sent := func(text string) bool {
		for _, m := range requests {
			if strings.HasPrefix(m, "[notice]") && strings.Contains(m, text) {
				return true
			}
		}
		return false
	}
	var notes []string // the open turn's queued notices, not yet sent
	var finals []string
	exit := 0
	for _, l := range lines {
		switch l.Kind {
		case "input":
			if l.Data["steer"] != nil {
				continue
			}
			r.turn = "user"
			if l.Data["wake"] == true {
				r.turn = "wake"
				if l.Data["reason"] == "notice" {
					r.wakes++
					r.model = append(r.model, bbjItems(l.Text)...)
					finals = append(finals, l.Text)
				}
			}
		case "job":
			switch l.Data["event"] {
			case "started":
				r.typed, r.job = "started", "running"
				r.until = l.Data["until"] != nil
			case "finished":
				r.typed, r.job = "finished", "finished"
				if n, ok := l.Data["exit"].(float64); ok {
					exit = int(n)
				}
			case nil:
				finals = append(finals, l.Text)
				switch {
				case r.turn == "none":
					r.quitLog = append(r.quitLog, bbjItems(l.Text)...)
				case sent(l.Text):
					r.model = append(r.model, bbjItems(l.Text)...)
				default:
					notes = append(notes, l.Text)
				}
			}
		case "cancelled", "done":
			if r.turn == "none" {
				continue // the done after a cancelled
			}
			r.last = r.turn
			if l.Kind == "cancelled" || l.Data["stop"] != nil {
				r.last = "cancelled"
			}
			for _, n := range notes {
				r.lost = append(r.lost, bbjItems(n)...)
			}
			r.turn, notes = "none", nil
		}
	}
	for _, n := range notes {
		r.queued = append(r.queued, bbjItems(n)...)
	}
	if r.job == "finished" {
		r.job = "finished (no finish notice)"
		for _, f := range finals {
			if k := bbjFinal(f); k != "" {
				r.job = k
			}
		}
		if exit == 0 && r.job != "exited" {
			r.job += " (exit 0)"
		}
	}
	return r
}

// read is the session's row and its history file as lines. serve's
// transcript leaves out the typed job entries, which the spec's job and
// typed are about, so the lines come from the file. A session stopped
// before it wrote anything is forgotten with its child: no row, no
// file, which reads as a child that is gone.
func (a *bbjAdapter) read() (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	var e *servetest.APIError
	if errors.As(err, &e) && e.Status == http.StatusNotFound {
		return serve.Row{}, nil, nil
	} else if err != nil {
		return row, nil, err
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return row, nil, err
	}
	lines := make([]serve.Line, 0, len(entries))
	for _, en := range entries {
		l := serve.Line{Seq: en.Seq, At: en.At, Kind: en.Kind, Data: map[string]any{}}
		for k, v := range en.Data {
			if k == "text" {
				l.Text, _ = v.(string)
			} else {
				l.Data[k] = v
			}
		}
		lines = append(lines, l)
	}
	return row, lines, nil
}

// wait polls the row and transcript until ok holds, for at most d.
func (a *bbjAdapter) wait(what string, d time.Duration, ok func(serve.Row, []serve.Line) bool) ([]serve.Line, error) {
	deadline := time.Now().Add(d)
	for {
		row, lines, err := a.read()
		if err == nil && ok(row, lines) {
			return lines, nil
		}
		if time.Now().After(deadline) {
			return lines, fmt.Errorf("waiting %s for %s: row %s live=%v, transcript %v (err %v)%s", d, what, row.Status, row.Live, bbjKinds(lines), err, bbjDump(lines))
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// bbjDump is the transcript in full, for a failed wait, when
// BBJ_DUMP is set.
func bbjDump(lines []serve.Line) string {
	if os.Getenv("BBJ_DUMP") == "" {
		return ""
	}
	var b strings.Builder
	for _, l := range lines {
		d, _ := json.Marshal(l.Data)
		fmt.Fprintf(&b, "\n  %s %q %s", l.Kind, l.Text, d)
	}
	return b.String()
}

func bbjKinds(lines []serve.Line) []string {
	var k []string
	for _, l := range lines {
		s := l.Kind
		if ev, ok := l.Data["event"].(string); ok {
			s += ":" + ev
		} else if l.Kind == "job" {
			s += ":note"
		}
		if l.Data["wake"] == true {
			s += ":wake"
		}
		k = append(k, s)
	}
	return k
}

// queue adds a turn to llm-control's queue under the walk's next name.
func (a *bbjAdapter) queue(turn control.Turn) string {
	a.seq++
	name := fmt.Sprintf("t%06d", a.seq)
	control.Queue(a.t, a.dir, name, turn)
	return name
}

func (a *bbjAdapter) block() string {
	return a.queue(control.Turn{Mode: "block", Text: "reply"})
}

// taken waits for the request that takes turn name.
func (a *bbjAdapter) taken(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			a.names = append(a.names, name)
			a.takes++
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("llm-control turn %s not taken after %s", name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// answer releases the held request as turn says and waits for the next
// request, which takes a fresh held turn.
func (a *bbjAdapter) answer(turn control.Turn) error {
	next := a.block()
	control.ReleaseWith(a.t, a.dir, a.held, turn)
	a.held = next
	return a.taken(next)
}

// pass is the gate, counting how deep each walk gets. A JobKill's notice
// lands on its own, so any step but NoticeDelivered after it leaves the
// real system.
func (a *bbjAdapter) pass(enabled bool, notice bool) bool {
	ok := a.gate.pass(enabled)
	if ok {
		a.depth++
		if a.landing && !notice {
			a.detach("a step between JobKill and its notice")
		}
	}
	return ok
}

func (a *bbjAdapter) detach(why string) {
	if !a.detached {
		a.detached, a.why = true, why
	}
}

func (a *bbjAdapter) real() bool { return !a.detached }

// script is the job: a loop the adapter steers through files in jobDir.
// The perl helper leaves the job's process group, so a kill leaves it
// alive, and holds the job's stdout until the shell is gone, a second
// longer when `slow` exists: the job's Wait returns, and its notice
// lands, only when the pipe closes. The loop ends when its bough child
// does, so a walk that ends mid-job leaves nothing running.
func (a *bbjAdapter) script() string {
	return fmt.Sprintf(`D=%q; P=$PPID
perl -e 'setpgrp(0,0); my $p = getppid(); while (kill(0, $p)) { select(undef, undef, undef, 0.02) } select(undef, undef, undef, 1.0) if -e "$ARGV[0]/slow"' "$D" &
while :; do
  if [ -e "$D/match" ] && [ ! -e "$D/matched" ]; then : > "$D/matched"; echo %s; fi
  [ -e "$D/exit" ] && exit 0
  kill -0 "$P" 2>/dev/null || exit 3
  sleep 0.02
done`, a.jobDir, bbjMatch)
}

func (a *bbjAdapter) touch(name string) error {
	return os.WriteFile(filepath.Join(a.jobDir, name), nil, 0o644)
}

// killJob is the Work dialog's Stop on job 1: serve's POST, which the
// child gets as "/jobkill 1".
func (a *bbjAdapter) killJob() error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/jobs/1/kill", nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("kill job 1: %s", resp.Status)
	}
	return nil
}

// trigger makes a deferred job event real.
func (a *bbjAdapter) trigger(ev string) error {
	switch ev {
	case "match":
		return a.touch("match")
	case "exit":
		if a.exitAsKill {
			return a.killJob()
		}
		return a.touch("exit")
	case "kill", "limit":
		return a.killJob()
	}
	return fmt.Errorf("no job event %q", ev)
}

func bbjCountLines(lines []serve.Line, ok func(serve.Line) bool) int {
	n := 0
	for _, l := range lines {
		if ok(l) {
			n++
		}
	}
	return n
}

func isNote(l serve.Line) bool { return l.Kind == "job" && l.Data["event"] == nil }

func isNoticeWake(l serve.Line) bool {
	return l.Kind == "input" && l.Data["wake"] == true && l.Data["reason"] == "notice"
}

// deliver makes the deferred job events real and waits for their one
// notice: a wake turn when none is open (its request takes a held
// turn), a queued note inside the turn in flight otherwise.
func (a *bbjAdapter) deliver() error {
	_, lines, err := a.read()
	if err != nil {
		return err
	}
	wakes, notes := bbjCountLines(lines, isNoticeWake), bbjCountLines(lines, isNote)
	idle := a.held == ""
	var w string
	if idle {
		w = a.block()
	}
	for _, ev := range a.deferred {
		if err := a.trigger(ev); err != nil {
			return err
		}
	}
	a.deferred = nil
	if idle {
		if _, err := a.wait("the notice to open a wake turn", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
			return bbjCountLines(l, isNoticeWake) > wakes
		}); err != nil {
			return err
		}
		a.held, a.takes = w, 0
		return a.taken(w)
	}
	_, err = a.wait("the notice to be queued in the open turn", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
		return bbjCountLines(l, isNote) > notes
	})
	return err
}

// ---- the person ----

func (a *bbjAdapter) Prompt() error {
	sh := &a.sh
	if !a.pass(sh.turn == "none" && sh.child == "alive" && sh.prompts < 1, false) {
		return nil
	}
	if a.real() {
		name := a.block()
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
			return err
		}
		a.held, a.takes = name, 0
		if err := a.taken(name); err != nil {
			return err
		}
	}
	sh.prompts++
	sh.turn = "user"
	return nil
}

func (a *bbjAdapter) Stop() error {
	sh := &a.sh
	if !a.pass(sh.child == "alive" && sh.stops < 1, false) {
		return nil
	}
	if len(sh.pending) > 0 {
		a.detach("Stop with a notice pending")
	}
	if a.real() {
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Interrupt(ctx, a.id); err != nil {
			return fmt.Errorf("stop: %w", err)
		}
		open, running := sh.open(), sh.job == "running"
		if _, err := a.wait("the stop to end the child", actionTimeout, func(r serve.Row, l []serve.Line) bool {
			if r.Live || (open && !lastCloseCancelled(l)) {
				return false
			}
			return !running || strings.Contains(lastNoteText(l), "killed when bough quit")
		}); err != nil {
			return err
		}
		a.held, a.ahead = "", "stop"
	}
	sh.stops++
	sh.child = "exiting"
	if sh.open() {
		sh.lost = append(sh.lost, sh.queued...)
		sh.queued = nil
		sh.turn = "cancelling"
	}
	return nil
}

func lastNoteText(lines []serve.Line) string {
	for i := len(lines) - 1; i >= 0; i-- {
		if isNote(lines[i]) {
			return lines[i].Text
		}
	}
	return ""
}

// KillJob is the Work dialog's Stop on the running job.
func (a *bbjAdapter) KillJob() error {
	sh := &a.sh
	if !a.pass(sh.child == "alive" && sh.job == "running", false) {
		return nil
	}
	if a.real() {
		a.deferred = append(a.deferred, "kill")
	}
	sh.finish("killed")
	return nil
}

// ---- the model ----

// BashBackground's until is the spec's `any` choice: the runner hands
// it over as the argument "u", a path as the state it expects after.
func (a *bbjAdapter) BashBackground(args []fmbt.Arg) error {
	sh := &a.sh
	if !a.pass(sh.open() && sh.child == "alive" && sh.job == "none", false) {
		return nil
	}
	until := rand.IntN(2) == 1
	if u, ok := a.want["Session#0.until"].(bool); ok {
		until = u
	}
	for _, arg := range args {
		if u, ok := arg.Value.(bool); ok && arg.Name == "u" {
			until = u
		}
	}
	if a.real() {
		args := map[string]any{"command": a.script(), "background": true, "timeout": "300s"}
		if until {
			args["until"] = bbjMatch
		}
		if err := a.answer(control.Turn{Mode: "call", Tool: "bash", Args: args}); err != nil {
			return err
		}
		if _, err := a.wait("the job's started entry", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
			return bbjCountLines(l, func(x serve.Line) bool { return x.Kind == "job" && x.Data["event"] == "started" }) > 0
		}); err != nil {
			return err
		}
	}
	sh.flush()
	sh.job, sh.typed, sh.until = "running", "started", until
	return nil
}

func (a *bbjAdapter) JobKill() error {
	sh := &a.sh
	if !a.pass(sh.open() && sh.child == "alive" && sh.job == "running", false) {
		return nil
	}
	if len(sh.pending) > 0 {
		a.detach("JobKill with a notice pending")
	}
	if a.real() {
		if err := a.touch("slow"); err != nil {
			return err
		}
		if err := a.answer(control.Turn{Mode: "call", Tool: "job_kill", Args: map[string]any{"id": 1}}); err != nil {
			return err
		}
		a.landing = true
	}
	sh.flush()
	sh.finish("killed")
	return nil
}

// Poll reads the job. A job call with the job still running waits up to
// 10 s for a change, so while it runs the model makes another call, a
// response like any other (the spec's Poll is only its boundary).
func (a *bbjAdapter) Poll() error {
	sh := &a.sh
	if !a.pass(sh.open() && sh.child == "alive" && sh.job != "none", false) {
		return nil
	}
	if a.real() {
		call := control.Turn{Mode: "call", Tool: "job", Args: map[string]any{"id": 1}}
		if sh.job == "running" || len(a.deferred) > 0 {
			call = control.Turn{Mode: "call", Tool: "bash", Args: map[string]any{"command": "true"}}
		}
		if err := a.answer(call); err != nil {
			return err
		}
	}
	sh.flush()
	return nil
}

func (a *bbjAdapter) Reply() error {
	sh := &a.sh
	if !a.pass(sh.open() && sh.child == "alive", false) {
		return nil
	}
	if a.real() {
		if len(sh.queued) > 0 {
			// The notices sent at this boundary keep the turn open.
			if err := a.answer(control.Turn{}); err != nil {
				return err
			}
		} else {
			control.Release(a.t, a.dir, a.held)
			a.held = ""
			if _, err := a.wait("the reply to close the turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
				return r.Status != serve.StatusRunning && bbjObserve(r, l, nil).turn == "none"
			}); err != nil {
				return err
			}
		}
	}
	if len(sh.queued) > 0 {
		sh.flush()
	} else {
		sh.last, sh.turn = sh.turn, "none"
	}
	return nil
}

// ---- the job ----

func (a *bbjAdapter) OutputMatchesUntil() error {
	sh := &a.sh
	if !a.pass(sh.job == "running" && sh.until && !sh.matchSeen(), false) {
		return nil
	}
	a.jobEvent("match")
	sh.pending = append(sh.pending, "match")
	return nil
}

func (a *bbjAdapter) JobExits() error {
	sh := &a.sh
	if !a.pass(sh.job == "running", false) {
		return nil
	}
	a.jobEvent("exit")
	sh.finish("exited")
	return nil
}

// jobEvent defers a job event to its NoticeDelivered; one while the
// child is already exiting would have to be real during an exit that
// takes milliseconds.
func (a *bbjAdapter) jobEvent(ev string) {
	if a.ahead == "stop" {
		a.detach("a job event while the child exits")
	}
	if a.real() {
		a.deferred = append(a.deferred, ev)
	}
}

// LimitExpires is the limit's timer, which fires at a time and not on
// cue. It is made real as a kill of the job: the same Wait, the same
// notice path, only the notice's reason differs ("killed after" the
// limit), so job is the adapter's record from here on (`limited`).
func (a *bbjAdapter) LimitExpires() error {
	sh := &a.sh
	if !a.pass(sh.job == "running", false) {
		return nil
	}
	a.jobEvent("limit")
	if a.real() {
		a.limited = true
	}
	sh.finish("limit")
	return nil
}

// ---- the engine and serve ----

// BudgetStop runs the turn into its step budget: the held request
// answers with a call, and padding calls follow until the gate refuses
// the next request. The cancel closes with it (CancelCloses).
func (a *bbjAdapter) BudgetStop() error {
	sh := &a.sh
	if !a.pass(sh.open() && sh.child == "alive" && sh.budget < 1, false) {
		return nil
	}
	if len(sh.queued) > 0 {
		a.detach("BudgetStop with notices queued")
	}
	if a.real() {
		pads := bbjMaxSteps - a.takes
		if pads < 0 {
			return fmt.Errorf("budget stop: the turn already made %d requests, over max_steps %d", a.takes, bbjMaxSteps)
		}
		var names []string
		for i := range pads {
			names = append(names, a.queue(control.Turn{Mode: "ok", Calls: []control.Call{{Name: "job_kill", Args: map[string]any{"id": 1000 + i}}}}))
		}
		control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: "job_kill", Args: map[string]any{"id": 999}})
		a.held = ""
		for _, n := range names {
			if err := a.taken(n); err != nil {
				return err
			}
		}
		if _, err := a.wait("the step budget to stop the turn", actionTimeout, func(r serve.Row, l []serve.Line) bool {
			o := bbjObserve(r, l, nil)
			return o.turn == "none" && o.last == "cancelled"
		}); err != nil {
			return err
		}
		a.ahead = "budget"
	}
	sh.budget++
	sh.lost = append(sh.lost, sh.queued...)
	sh.queued = nil
	sh.turn = "cancelling"
	return nil
}

func (a *bbjAdapter) NoticeDelivered() error {
	sh := &a.sh
	if !a.pass(sh.child != "gone" && len(sh.pending) > 0 && !(sh.turn == "cancelling" && sh.waits), true) {
		return nil
	}
	switch {
	case a.landing:
		if len(sh.pending) > 1 {
			a.detach("two notices delivered at once")
		}
		if a.real() {
			a.landing = false
			if _, err := a.wait("the killed job's notice to be queued", actionTimeout, func(_ serve.Row, l []serve.Line) bool {
				return strings.Contains(lastNoteText(l), "job 1 [")
			}); err != nil {
				return err
			}
		}
	case sh.turn == "cancelling":
		// Held behind the cancel: the real cancel has closed (ahead), and
		// the events stay deferred until CancelCloses takes them.
	default:
		if len(sh.pending) > 1 {
			a.detach("two notices delivered at once")
		}
		if a.real() {
			if err := a.deliver(); err != nil {
				return err
			}
		}
	}
	if sh.turn == "cancelling" {
		sh.waits = true
	} else if sh.turn == "none" {
		sh.model = append(sh.model, sh.pending...)
		sh.pending = nil
		sh.wakes++
		sh.turn = "wake"
	} else {
		sh.queued = append(sh.queued, sh.pending...)
		sh.pending = nil
	}
	return nil
}

func (a *bbjAdapter) CancelCloses() error {
	sh := &a.sh
	if !a.pass(sh.turn == "cancelling", false) {
		return nil
	}
	sh.turn, sh.last = "none", "cancelled"
	wake := false
	if sh.waits {
		sh.waits = false
		if len(sh.pending) > 0 {
			wake = true
			sh.model = append(sh.model, sh.pending...)
			sh.pending = nil
			sh.wakes++
			sh.turn = "wake"
		}
	}
	if a.ahead == "budget" {
		a.ahead = ""
		if wake {
			if len(a.deferred) > 1 {
				a.detach("two notices delivered at once")
			}
			if a.real() {
				return a.deliver()
			}
		}
	}
	return nil
}

func (a *bbjAdapter) ChildExits() error {
	sh := &a.sh
	if !a.pass(sh.child == "exiting" && sh.turn != "cancelling", false) {
		return nil
	}
	if a.ahead == "stop" {
		a.ahead = "" // the server did this at Stop; its state is the spec's again
	}
	if sh.turn == "wake" {
		sh.turn, sh.last = "none", "cancelled"
	}
	if sh.job == "running" {
		sh.finish("quit")
	}
	sh.lost = append(sh.lost, sh.queued...)
	sh.queued = nil
	sh.quitLog = append(sh.quitLog, sh.pending...)
	sh.pending = nil
	sh.waits = false
	sh.child = "gone"
	return nil
}

var bbjActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":  action((*bbjAdapter).Prompt),
	"Stop":    action((*bbjAdapter).Stop),
	"KillJob": action((*bbjAdapter).KillJob),
	"BashBackground": func(m any, args []fmbt.Arg) (any, error) {
		return nil, m.(*bbjAdapter).BashBackground(args)
	},
	"JobKill":            action((*bbjAdapter).JobKill),
	"Poll":               action((*bbjAdapter).Poll),
	"Reply":              action((*bbjAdapter).Reply),
	"OutputMatchesUntil": action((*bbjAdapter).OutputMatchesUntil),
	"JobExits":           action((*bbjAdapter).JobExits),
	"LimitExpires":       action((*bbjAdapter).LimitExpires),
	"BudgetStop":         action((*bbjAdapter).BudgetStop),
	"NoticeDelivered":    action((*bbjAdapter).NoticeDelivered),
	"CancelCloses":       action((*bbjAdapter).CancelCloses),
	"ChildExits":         action((*bbjAdapter).ChildExits),
}, "": {
	// The spec turns deadlock detection off, so fizz links a state with
	// nothing enabled (bounds used up, the child gone) to itself as a
	// role-less "end". A path takes it as a step where nothing happens;
	// the runner offers it everywhere, and a taken "end" matches no link,
	// so there it closes the gate like any disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*bbjAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func bbjOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
}

// compare checks the adapter's state against a path's qualified state.
func (a *bbjAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Session#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	if len(diff) > 0 {
		sort.Strings(diff)
		return fmt.Errorf("state: %s", strings.Join(diff, "; "))
	}
	return nil
}

type bbjPath struct {
	Trace []tracecheck.Step `json:"trace"`
	// Strict: a steerable walk, which must never leave the real system.
	Strict bool `json:"-"`
}

func bbjPaths(t *testing.T, cover tracecheck.Cover) []bbjPath {
	t.Helper()
	b, err := pathsJSONCover("background_bash_jobs", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []bbjPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	return doc.Paths
}

// bbjSteerable is walks over the part of the graph the adapter can steer
// the real system through: every link the adapter's detach rules allow
// (bbjSteers), from Init. pathsJSON's walks cover the whole graph and
// leave the real system at the first interleaving it cannot be steered
// into, which hides every state behind one from the server; these reach
// each state the server can be put in and compare it there. A walk here
// that detaches is a disagreement between bbjSteers and the adapter.
func bbjSteerable(t *testing.T, cover tracecheck.Cover) []bbjPath {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("background_bash_jobs")), "..", "testdata", "background_bash_jobs"))
	if err != nil {
		t.Fatal(err)
	}
	out := map[int][]int{}
	for i, l := range g.Links {
		out[l.Src] = append(out[l.Src], i)
	}
	// settle follows fork links (BashBackground's until) to settled nodes.
	var settle func(n int) []int
	settle = func(n int) []int {
		if g.Nodes[n].Name == "yield" {
			return []int{n}
		}
		var ys []int
		for _, i := range out[n] {
			if g.Links[i].Type != "action" {
				ys = append(ys, settle(g.Links[i].Dest)...)
			}
		}
		if ys == nil {
			return []int{n}
		}
		return ys
	}
	type edge struct {
		name string
		dest int
	}
	edges := map[int][]edge{}
	for n := range g.Nodes {
		for _, i := range out[n] {
			if l := g.Links[i]; l.Type == "action" {
				for _, y := range settle(l.Dest) {
					edges[n] = append(edges[n], edge{strings.TrimPrefix(l.Name, "Session#0."), y})
				}
			}
		}
	}
	// A product node: a graph node, and whether a JobKill's notice is on
	// its way (the adapter's landing), which only NoticeDelivered may
	// follow.
	type pnode struct {
		n       int
		landing bool
	}
	next := func(p pnode, e edge) (pnode, bool) {
		if !bbjSteers(g.Nodes[p.n].State, p.landing, e.name) {
			return pnode{}, false
		}
		return pnode{e.dest, e.name == "JobKill"}, true
	}
	key := func(p pnode, e edge) string { return fmt.Sprint(p.n, e.name, e.dest) }
	want := map[string]bool{}
	seen := map[pnode]bool{{0, false}: true}
	for frontier := []pnode{{0, false}}; len(frontier) > 0; {
		var more []pnode
		for _, p := range frontier {
			if cover == tracecheck.CoverStates && p.n != 0 {
				want[fmt.Sprint(p.n)] = true
			}
			for _, e := range edges[p.n] {
				q, ok := next(p, e)
				if !ok {
					continue
				}
				if cover == tracecheck.CoverTransitions {
					want[key(p, e)] = true
				}
				if !seen[q] {
					seen[q] = true
					more = append(more, q)
				}
			}
		}
		frontier = more
	}
	type hop struct {
		from pnode
		e    edge
	}
	// nearest is the shortest chain of steerable hops from cur to a
	// wanted state, or to a node with a wanted link out (plus it).
	nearest := func(cur pnode) []hop {
		parent := map[pnode]hop{}
		seen := map[pnode]bool{cur: true}
		chain := func(p pnode) []hop {
			var hs []hop
			for p != cur {
				h := parent[p]
				hs = append([]hop{h}, hs...)
				p = h.from
			}
			return hs
		}
		for frontier := []pnode{cur}; len(frontier) > 0; {
			var more []pnode
			for _, p := range frontier {
				if cover == tracecheck.CoverStates && p != cur && want[fmt.Sprint(p.n)] {
					return chain(p)
				}
				for _, e := range edges[p.n] {
					q, ok := next(p, e)
					if !ok {
						continue
					}
					if cover == tracecheck.CoverTransitions && want[key(p, e)] {
						return append(chain(p), hop{p, e})
					}
					if !seen[q] {
						seen[q] = true
						parent[q] = hop{p, e}
						more = append(more, q)
					}
				}
			}
			frontier = more
		}
		return nil
	}
	var paths []bbjPath
	for {
		cur := pnode{0, false}
		trace := []tracecheck.Step{{Action: "Init", State: g.Nodes[0].State}}
		for len(trace) <= 50 {
			hs := nearest(cur)
			if len(hs) == 0 {
				break
			}
			for _, h := range hs {
				delete(want, key(h.from, h.e))
				delete(want, fmt.Sprint(h.e.dest))
				trace = append(trace, tracecheck.Step{Action: "Session#0." + h.e.name, State: g.Nodes[h.e.dest].State})
			}
			last := hs[len(hs)-1]
			cur, _ = next(last.from, last.e)
		}
		if len(trace) == 1 {
			break
		}
		paths = append(paths, bbjPath{Trace: trace})
	}
	if len(want) > 0 {
		t.Fatalf("steerable walks left %d targets uncovered", len(want))
	}
	return paths
}

// bbjSteers is the adapter's detach rules as a predicate on a link: the
// spec state it leaves (qualified names), whether a JobKill's notice is
// on its way, and the action. The adapter detaches exactly where this
// says no.
func bbjSteers(s map[string]any, landing bool, action string) bool {
	n := func(f string) int { l, _ := s["Session#0."+f].([]any); return len(l) }
	if landing && action != "NoticeDelivered" {
		return false
	}
	switch action {
	case "Stop", "JobKill":
		return n("pending") == 0
	case "OutputMatchesUntil", "JobExits", "LimitExpires":
		return s["Session#0.child"] != "exiting"
	case "NoticeDelivered":
		return s["Session#0.turn"] == "cancelling" || n("pending") <= 1
	case "CancelCloses":
		return s["Session#0.waits"] != true || n("pending") <= 1
	case "BudgetStop":
		return n("queued") == 0
	}
	return true
}

// walk drives paths through the adapter, comparing its state with the
// path's after every step. first stops at the first mismatch; strict
// counts a walk that leaves what the adapter drives as a mismatch.
func (a *bbjAdapter) walkPaths(t *testing.T, paths []bbjPath, first bool) (mismatches []string) {
	t.Helper()
	for pi, p := range paths {
		var names []string
		before := len(mismatches)
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			names = append(names, name)
			a.want = step.State
			var err error
			if si == 0 {
				err = a.Init()
			} else if name == "end" {
				// Nothing is enabled: the state stays as it is.
			} else {
				f, ok := bbjActions["Session"][name]
				if !ok {
					t.Fatalf("path %d: no adapter action %q", pi, name)
				}
				_, err = f(a, nil)
			}
			if err == nil {
				err = a.compare(step.State)
			}
			if err != nil {
				mismatches = append(mismatches, fmt.Sprintf("path %d step %d (%s): %v", pi, si, strings.Join(names, " "), err))
				break
			}
			if !a.detached && a.ahead == "" {
				if a.checked == nil {
					a.checked = map[string]bool{}
				}
				a.checked[fmt.Sprint(step.State)] = true
			}
		}
		if p.Strict && a.detached && len(mismatches) == before {
			mismatches = append(mismatches, fmt.Sprintf("steerable path %d (%s) left what the adapter drives: %s", pi, strings.Join(names, " "), a.why))
		}
		a.want = nil
		if err := a.Cleanup(); err != nil {
			t.Fatalf("path %d: cleanup: %v", pi, err)
		}
		if first && len(mismatches) > 0 {
			break
		}
	}
	return mismatches
}

// bbjShards is how many serves split the walks: each walk is a session
// and its child, most of the time is waiting on them, and one serve
// took minutes for the cover.
const bbjShards = 4

func TestBackgroundBashJobs(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := bbjPaths(t, envCover())
	steer := bbjSteerable(t, envCover())
	for i := range steer {
		steer[i].Strict = true
	}
	paths = append(paths, steer...)
	t.Logf("background_bash_jobs: %d steerable walks", len(steer))
	var mu sync.Mutex
	detached, checked, all := map[string]int{}, map[string]bool{}, map[string]bool{}
	for _, p := range paths {
		for _, s := range p.Trace {
			all[fmt.Sprint(s.State)] = true
		}
	}
	// The group returns once every shard has.
	t.Run("walks", func(t *testing.T) {
		for sh := range bbjShards {
			t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
				t.Parallel()
				var mine []bbjPath
				for i := sh; i < len(paths); i += bbjShards {
					mine = append(mine, paths[i])
				}
				a := newBBJAdapter(t)
				for _, m := range a.walkPaths(t, mine, false) {
					t.Error(m)
				}
				g, err := tracecheck.Load(fizzCheck(t, "background_bash_jobs"))
				if err != nil {
					t.Fatal(err)
				}
				for _, id := range a.ids {
					// A session stopped before its first prompt has no file.
					if _, err := os.Stat(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")); err == nil {
						checkHistory(t, g, sessionHistory(t, a.s.Home, id), bbjHistory)
					}
				}
				mu.Lock()
				for k, v := range a.detachedBy {
					detached[k] += v
				}
				for k := range a.checked {
					checked[k] = true
				}
				mu.Unlock()
				t.Logf("shard %d: %d paths, walks by depth %v", sh, len(mine), a.depths)
			})
		}
	})
	t.Logf("background_bash_jobs: %d paths; %d of the %d states they reach compared against the server; walks that left what the adapter drives, by why: %v",
		len(paths), len(checked), len(all), detached)
	t.Run("runner", func(t *testing.T) {
		a := newBBJAdapter(t)
		err := runMBT(t, "background_bash_jobs", a, bbjActions, bbjOptions())
		t.Logf("background_bash_jobs runner: walks by depth %v, left what the adapter drives: %v", a.depths, a.detachedBy)
		if err != nil {
			t.Errorf("model-based run: %v", err)
		}
	})
}

// The job's exit made real as a kill must fail the walks: the notice
// says the job was killed where the spec says it exited 0. The walk that
// shows it is any JobExits then NoticeDelivered, which the state cover
// reaches in its first few paths.
func TestBackgroundBashJobsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newBBJAdapter(t)
	a.exitAsKill = true
	m := a.walkPaths(t, bbjPaths(t, tracecheck.CoverStates), true)
	if len(m) == 0 {
		t.Fatal("paths whose JobExits kills the job passed; the adapter is not checking state")
	}
	t.Logf("caught: %s", m[0])
}

// bbjHistory reads the abstract trace off a transcript. The person's
// kill and the model's job_kill both read as KillJob (the finished entry
// does not say who), a match is placed just before the notice that
// carries it, a queued notice still unsent when its turn ends normally
// went out at a Reply, and a stop or a budget stop reads as the step
// and its CancelCloses at the entry that closes the turn.
func bbjHistory(entries []history.Entry) []tracecheck.Step {
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: state}
	}
	turn := func(s string) map[string]any { return map[string]any{"Session#0.turn": s} }
	steps := []tracecheck.Step{{Action: "Init", State: turn("none")}}
	open, queued, stopped, exited := "", 0, false, false
	final := func(i int) string {
		for _, e := range entries[i:] {
			text, _ := e.Data["text"].(string)
			if k := bbjFinal(text); k != "" && (e.Kind == "input" || (e.Kind == "job" && e.Data["event"] == nil)) {
				return k
			}
		}
		return ""
	}
	matches := func(text string) {
		for _, it := range bbjItems(text) {
			if it == "match" {
				steps = append(steps, step("OutputMatchesUntil", nil))
			}
		}
	}
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			if e.Data["steer"] != nil {
				continue
			}
			if e.Data["wake"] == true {
				matches(text)
				steps = append(steps, step("NoticeDelivered", turn("wake")))
				open = "wake"
				continue
			}
			steps = append(steps, step("Prompt", turn("user")))
			open = "user"
		case "job":
			switch e.Data["event"] {
			case "started":
				until := e.Data["until"] != nil
				steps = append(steps, step("BashBackground", map[string]any{"Session#0.job": "running", "Session#0.until": until}))
				queued = 0
			case "finished":
				switch final(i) {
				case "exited":
					steps = append(steps, step("JobExits", map[string]any{"Session#0.job": "exited"}))
				case "killed":
					steps = append(steps, step("KillJob", map[string]any{"Session#0.job": "killed"}))
				case "limit":
					steps = append(steps, step("LimitExpires", map[string]any{"Session#0.job": "limit"}))
				}
			case nil:
				if open != "" {
					matches(text)
					steps = append(steps, step("NoticeDelivered", nil))
					queued++
				} else if !exited {
					// stopJobs at exit: one entry per notice, one exit.
					if !stopped {
						steps = append(steps, step("Stop", nil))
						stopped = true
					}
					steps = append(steps, step("ChildExits", map[string]any{"Session#0.child": "gone"}))
					exited = true
				}
			}
		case "cancelled":
			if open != "" {
				steps = append(steps, step("Stop", nil), step("CancelCloses", map[string]any{"Session#0.turn": "none", "Session#0.last": "cancelled"}))
				open, queued, stopped = "", 0, true
			}
		case "done":
			if open == "" {
				continue
			}
			if e.Data["stop"] != nil {
				steps = append(steps, step("BudgetStop", nil), step("CancelCloses", map[string]any{"Session#0.turn": "none", "Session#0.last": "cancelled"}))
			} else {
				if queued > 0 {
					steps = append(steps, step("Reply", turn(open)))
				}
				steps = append(steps, step("Reply", map[string]any{"Session#0.turn": "none", "Session#0.last": open}))
			}
			open, queued = "", 0
		}
	}
	return steps
}

func init() { historyProjections["background_bash_jobs"] = bbjHistory }

// bbjHistory's projection of a transcript the walks write (an until
// match and then the exit, each waking the idle agent) is a path in the
// graph, and the same transcript without its done lines is not: the
// trace check can fail. The walks check their real transcripts too.
func TestBackgroundBashJobsHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("background_bash_jobs")), "..", "testdata", "background_bash_jobs"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string, data map[string]any) history.Entry { return history.Entry{Kind: kind, Data: data} }
	entries := []history.Entry{
		e("meta", map[string]any{}),
		e("input", map[string]any{"text": "turn t000001"}),
		e("job", map[string]any{"id": 1.0, "event": "started", "cmd": "loop", "until": bbjMatch}),
		e("done", map[string]any{}),
		e("input", map[string]any{"text": "[background job] …\n\njob 1 matched \"BBJ-MATCH\" while running: loop\nBBJ-MATCH", "wake": true, "reason": "notice"}),
		e("done", map[string]any{"wake": true}),
		e("job", map[string]any{"id": 1.0, "event": "finished", "cmd": "loop", "exit": 0.0}),
		e("input", map[string]any{"text": "[background job] …\n\njob 1 [exited 0] loop (1s)", "wake": true, "reason": "notice"}),
		e("done", map[string]any{"wake": true}),
	}
	if v := g.Check(bbjHistory(entries)); v != nil {
		t.Fatalf("a transcript the walks write is not a path: %v", v)
	}
	var cut []history.Entry
	for _, en := range entries {
		if en.Kind != "done" {
			cut = append(cut, en)
		}
	}
	v := g.Check(bbjHistory(cut))
	if v == nil {
		t.Fatal("a transcript whose turns never close passed the trace check")
	}
	t.Logf("rejected: %v", v)
}
