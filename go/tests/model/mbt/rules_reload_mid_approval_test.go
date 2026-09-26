//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/rules_reload_mid_approval.fizz against a real serve: a config
// reload (the ask row's timeout_minutes, which cascades to remount the
// rules row: it Gets "ask-answers") disposes the rules row's policy
// (SetPolicyContext(nil)) before the new one lands, and a bash call
// racing that gap must queue behind it, not run unchecked (the fix in
// plugins/tools/tools.go: awaitPolicy). BOUGH_TEST_RULES_RELOAD_GATE holds the
// dispose exactly at that gap (internal/stepgate; plugins/rules.go's
// policer effect) until ReloadFinish lets it through, which is also
// when the rules file's decision actually flips (a person editing
// bough.yml and .codex/rules together, as the spec's header says).
//
// What is read off the server: status (idle/running/needs-you/done —
// this spec does not distinguish a stopped turn from a finished one,
// so both StatusStopped and StatusDone read as "done"); c1 (the one
// bash call's ask entry, its answer or end, or "queued" while it has
// been requested but nothing shows yet); shown (row.Ask matches our
// marker); armed is always read equal to shown (the spec's
// ArmedMatchesShown holds by construction: nothing here ever shows
// without arming, or arms without showing). reloading, version and
// locked are the adapter's own bookkeeping, mirroring the spec exactly
// (a config reload and a rule-file rewrite are not otherwise visible
// on the row).

// rmaRulePath is where the flipping Codex rule lives.
const rmaRulePath = ".codex/rules/rma.rules"

// rmaDecision is the rule's decision for a version: even lands
// "prompt" (an ask), odd "forbidden" (refused at once, no ask) — the
// spec's ReloadFinish.
func rmaDecision(version int) string {
	if version%2 == 0 {
		return "prompt"
	}
	return "forbidden"
}

func rmaRuleBody(version int) string {
	return fmt.Sprintf(`prefix_rule(pattern = ["echo", "rmatest"], decision = %q)`+"\n", rmaDecision(version))
}

// rmaConfig pins the loop row to code mode (llm-control's "block" turns
// are the Stream request code mode makes, not the engine's, so this
// flow's bash call — a plain tools.bash(...) js block — never gets
// taken under the default engine-unreal row) and, while disabled is
// true, disables the rules row: that unmounts it (kernel/reconcile.go
// drops a row whose new spec says disabled), running its dispose
// effect — SetPolicyContext(nil) — exactly as a real config reload
// that remounts it would (rules.go's own comment: "registers the gate
// once, per mount"). Re-enabling it is the other half of the same
// bough.yml edit landing. gen bumps the ask row's timeout_minutes in
// the same edit: the ask row's own remount disposes its Asker
// instance, which cancels whatever ask it currently holds at once
// (ask package, TestDisposedAskerCancelsItsAsks) — the spec's other
// finding, that an ask already open when a reload starts ends right
// then, not because the rules row's disable touches it (it does not:
// rules.findAsk looks the ask row up fresh on every approval, so it
// is only ever the ask row's own reload that can cancel one it is
// holding).
func rmaConfig(disabled bool, gen int) string {
	d := ""
	if disabled {
		d = "\n  disabled: true"
	}
	return controlConfig +
		"- id: loop\n  plugin: loop\n" +
		"- id: rules\n  plugin: rules" + d + "\n" +
		fmt.Sprintf("- id: ask\n  plugin: ask\n  config:\n    timeout_minutes: %d\n", 10+gen)
}

type rmaAdapter struct {
	t       *testing.T
	s       *servetest.Server
	dir     string // llm-control's queue
	gateDir string // BOUGH_TEST_RULES_RELOAD_GATE
	expire  string // BOUGH_TEST_ASK_EXPIRE_DIR
	gate    gate

	id, cwd string
	ids     []string
	walk    int
	turn    int
	q       int // turn names

	queued []string // the parent's block turns, not yet taken
	next   string
	held   []string // parent requests taken and not yet answered by Finish

	version   int
	gen       int // ask row's timeout_minutes generation, bumped every reload
	reloading bool
	requested bool
	locked    int
	stopping  bool

	// gateMisses is the wrong-adapter test's bug: ReloadFinish reports
	// no version change even though the row really did remount (the
	// rule file is still rewritten), so this adapter's own bookkeeping
	// disagrees with the one observable field the spec's ReloadFinish
	// always changes.
	gateMisses bool
}

func newRmaAdapter(t *testing.T) *rmaAdapter {
	gateDir := t.TempDir()
	expire := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: rmaConfig(false, 0),
		Files:  map[string]string{rmaRulePath: rmaRuleBody(0)},
		Env: []string{
			"BOUGH_TEST_RULES_RELOAD_GATE=" + gateDir,
			"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire,
		},
	})
	return &rmaAdapter{t: t, s: s, dir: control.Dir(s.Home), gateDir: gateDir, expire: expire, locked: -1}
}

func (a *rmaAdapter) marker() string { return fmt.Sprintf("w%dt%d", a.walk, a.turn) }

func (a *rmaAdapter) cmd() string { return "echo rmatest " + a.marker() }

func (a *rmaAdapter) queueNext() {
	a.q++
	name := fmt.Sprintf("t%06d", a.q)
	a.queued = append(a.queued, name)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "Done."})
	a.next = a.queued[0]
}

func (a *rmaAdapter) absorb() bool {
	took := false
	for len(a.queued) > 0 {
		if _, err := os.Stat(filepath.Join(a.dir, a.queued[0]+".taken")); err != nil {
			break
		}
		a.held = append(a.held, a.queued[0])
		a.queued = a.queued[1:]
		took = true
	}
	if took {
		a.queueNext()
	}
	return took
}

func (a *rmaAdapter) release(turn control.Turn) error {
	if len(a.held) == 0 {
		return errors.New("no model request is held")
	}
	name := a.held[len(a.held)-1]
	a.held = a.held[:len(a.held)-1]
	control.ReleaseWith(a.t, a.dir, name, turn)
	return nil
}

func (a *rmaAdapter) newTurn() {
	a.requested = false
}

// writeConfig replaces the overlay: it never waits for the reload to
// land, because our own gate (below) holds the rules row's dispose
// mid-reconcile, and that reconcile call is what emits "bough:
// reloaded" — waiting here would deadlock on ourselves.
func (a *rmaAdapter) writeConfig(disabled bool) error {
	path := filepath.Join(a.s.Home, ".bough", "bough.yml")
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(rmaConfig(disabled, a.gen)), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func (a *rmaAdapter) writeRules(version int) error {
	path := filepath.Join(a.s.Home, rmaRulePath)
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(rmaRuleBody(version)), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// waitGateHeld blocks until the rules row's dispose has actually
// reached the injected hold (internal/stepgate announces it as
// <dir>/held): the reload watcher debounces file writes,
// so a Bash right after writeConfig could otherwise race a reconcile
// that has not started yet.
func (a *rmaAdapter) waitGateHeld(d time.Duration) error {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(filepath.Join(a.gateDir, "held")); err == nil {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return errors.New("the rules row never reached the reload gate")
}

// waitReloaded waits for the child to say it reloaded, stamped after
// since: "bough: reloaded" (cmd/bough/main.go) is only logged once
// ctx.Reconcile returns, so it never fires while our gate holds one
// mid-reconcile — a caller waiting on it must release the gate first.
func (a *rmaAdapter) waitReloaded(since time.Time) error {
	ctx, cancel := actionCtx()
	defer cancel()
	st, err := a.s.Events(ctx, a.id)
	if err != nil {
		return err
	}
	defer st.Close()
	_, err = st.WaitFor(ctx, func(ev serve.Event) bool {
		return strings.Contains(ev.Text, "bough: reloaded") && ev.At.After(since)
	})
	if err != nil {
		return fmt.Errorf("waiting for the child to reload: %w", err)
	}
	return nil
}

// finishReload lets the held dispose (and the reconcile that disabled
// the rules row) finish, then writes a second edit re-enabling it: the
// kernel rereads bough.yml on each reconcile, so a row's spec toggled
// back only takes effect once the file says so again — the gate only
// ever delays finishing the FIRST edit's reconcile, not what the next
// one reads.
func (a *rmaAdapter) finishReload() error {
	t0 := time.Now()
	if err := os.WriteFile(filepath.Join(a.gateDir, "go"), nil, 0o644); err != nil {
		return err
	}
	if err := a.waitReloaded(t0); err != nil {
		return err
	}
	// The gate files are per-reload: clear them (and disarm) so the
	// next round's waitGateHeld does not see a stale "held" from this
	// one, and no unrelated later dispose blocks on a gate we forgot
	// to disarm.
	os.Remove(filepath.Join(a.gateDir, "go"))
	os.Remove(filepath.Join(a.gateDir, "held"))
	os.Remove(filepath.Join(a.gateDir, "done"))
	os.Remove(filepath.Join(a.gateDir, "armed"))
	t1 := time.Now()
	if err := a.writeConfig(false); err != nil {
		return err
	}
	return a.waitReloaded(t1)
}

func (a *rmaAdapter) Init() error {
	a.walk++
	a.turn = 0
	a.version, a.reloading, a.stopping, a.locked = 0, false, false, -1
	a.held = nil
	a.newTurn()
	a.gate.reset()
	if err := a.writeRules(0); err != nil {
		return err
	}
	// A walk before this one may have left the rules row disabled (its
	// generated sequence stopped mid-reload, never reaching
	// ReloadFinish): put it back before the new walk starts, but only
	// then — an identical rewrite on every walk is a needless extra
	// reconcile the fresh boot never needed.
	if cur, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "bough.yml")); err == nil && strings.Contains(string(cur), "disabled: true") {
		if err := a.writeConfig(false); err != nil {
			return err
		}
	}
	a.queueNext()
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	_, err = waitRow(a.s, a.id, "idle", func(r serve.Row) bool { return r.Status == serve.StatusIdle })
	return err
}

// Cleanup ends the walk's child and takes back what it left queued or
// held; a gate a walk left holding a reload is released so the next
// walk's serve is not stuck.
func (a *rmaAdapter) Cleanup() error {
	os.WriteFile(filepath.Join(a.gateDir, "go"), nil, 0o644)
	ctx, cancel := actionCtx()
	defer cancel()
	if a.id != "" {
		a.s.Archive(ctx, a.id)
	}
	for _, h := range a.held {
		control.Release(a.t, a.dir, h)
	}
	a.held = nil
	for _, name := range a.queued {
		os.Remove(filepath.Join(a.dir, name+".json"))
	}
	a.queued, a.next = nil, ""
	os.Remove(filepath.Join(a.gateDir, "go"))
	os.Remove(filepath.Join(a.gateDir, "held"))
	os.Remove(filepath.Join(a.gateDir, "done"))
	os.Remove(filepath.Join(a.gateDir, "armed"))
	return nil
}

func (a *rmaAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

type rmaView struct {
	row   serve.Row
	c1    string
	shown bool
	askID string
}

func (a *rmaAdapter) view() (rmaView, error) {
	var v rmaView
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil && !os.IsNotExist(err) {
		return v, err
	}
	start, closed := -1, true
	for i, e := range entries {
		switch e.Kind {
		case "input":
			start, closed = i, false
		case "done", "cancelled":
			closed = true
		}
	}
	m := a.marker()
	askID, answered, ended := "", false, false
	if start >= 0 {
		for i := start; i < len(entries); i++ {
			e := entries[i]
			switch {
			case e.Kind == "ask" && entryHas(e, m):
				askID, answered, ended = str(e.Data["id"]), false, false
			case e.Kind == "ask/answer" && askID != "" && str(e.Data["id"]) == askID:
				answered = true
			case e.Kind == "ask/end" && askID != "" && str(e.Data["id"]) == askID:
				ended = true
			case callEnd(e) && entryHas(e, m):
				ended = true
			}
		}
	}
	switch {
	case closed:
		v.c1 = ""
	case askID != "" && (answered || ended):
		v.c1 = "end"
	case askID != "":
		v.c1 = "open"
	case a.requested && ended:
		v.c1 = "end"
	case a.requested:
		v.c1 = "queued"
	default:
		v.c1 = ""
	}
	v.shown = row.Ask != nil
	v.askID = askID
	return v, nil
}

// status reads row.Status the way this spec sees it: it does not
// distinguish a stopped turn from a finished one, so both collapse to
// "done".
func rmaStatus(row serve.Row) string {
	switch row.Status {
	case serve.StatusIdle:
		return "idle"
	case serve.StatusRunning:
		return "running"
	case serve.StatusNeedsYou:
		return "needs-you"
	default:
		return "done"
	}
}

func (a *rmaAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"status":    rmaStatus(v.row),
		"reloading": a.reloading,
		"version":   a.version,
		"c1":        v.c1,
		"locked":    a.locked,
		"shown":     v.shown,
		"armed":     v.shown,
	}, nil
}

// waitFor polls the view until ok holds, for at most d; it never
// fails: the state check after the step says what did not happen.
func (a *rmaAdapter) waitFor(d time.Duration, ok func(rmaView) bool) {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		a.absorb()
		if v, err := a.view(); err == nil && ok(v) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// settle waits until the transcript, the row and the llm queue have
// been still for a moment, absorbing any request the parent made.
func (a *rmaAdapter) settle() error {
	const quiet = 300 * time.Millisecond
	deadline := time.Now().Add(actionTimeout)
	last, lastAt := "", time.Now()
	for time.Now().Before(deadline) {
		if a.absorb() {
			lastAt = time.Now()
		}
		ctx, cancel := actionCtx()
		row, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		sig := fmt.Sprintf("%s %v %d %d", row.Status, row.Live, len(lines), len(a.held))
		if sig != last {
			last, lastAt = sig, time.Now()
		} else if time.Since(lastAt) >= quiet {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("the session did not settle in %s", actionTimeout)
}

func (a *rmaAdapter) post(ctx context.Context, verb string, body map[string]string) (int, string, error) {
	b, _ := json.Marshal(body)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+a.id+"/"+verb, strings.NewReader(string(b)))
	if err != nil {
		return 0, "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	var e struct {
		Error string `json:"error"`
	}
	json.NewDecoder(resp.Body).Decode(&e)
	return resp.StatusCode, e.Error, nil
}

// --- the person, between turns ---

func (a *rmaAdapter) Prompt() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := rmaStatus(v.row)
	if !a.gate.pass(st == "idle" || st == "done") {
		return nil
	}
	a.turn++
	a.newTurn()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, fmt.Sprintf("go w%dt%d", a.walk, a.turn)); err != nil {
		return err
	}
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.absorb()
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	return a.settle()
}

// ReloadStart disables the rules row (its dispose sets the command
// policy nil and blocks on our gate right there, matching this
// action's self.reloading = True) and bumps the ask row's generation
// in the same edit: the ask row's own remount disposes its Asker,
// cancelling an ask already open at once.
func (a *rmaAdapter) ReloadStart() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.reloading) {
		return nil
	}
	wasOpen := v.c1 == "open"
	a.reloading = true
	a.gen++
	// Arm the gate just before the write that disposes the rules row,
	// so only that dispose (not one from unrelated dependency churn)
	// ever blocks.
	if err := os.WriteFile(filepath.Join(a.gateDir, "armed"), nil, 0o644); err != nil {
		return err
	}
	if err := a.writeConfig(true); err != nil {
		return err
	}
	if err := a.waitGateHeld(actionTimeout); err != nil {
		return err
	}
	if wasOpen {
		a.waitFor(5*time.Second, func(v rmaView) bool { return v.c1 != "open" })
	}
	return a.settle()
}

// ReloadFinish flips the version and rewrites the rule file to match,
// then lets the held dispose (and the new Apply behind it) through.
// A call that was queued behind the gap is decided against whichever
// rule just landed, the moment the policy reappears (plugins/tools
// Stats.awaitPolicy).
func (a *rmaAdapter) ReloadFinish() error {
	if !a.gate.pass(a.reloading) {
		return nil
	}
	a.reloading = false
	if !a.gateMisses {
		a.version = 1 - a.version
	}
	wasQueued := a.requested
	if wasQueued {
		a.locked = a.version
	}
	if err := a.writeRules(a.version); err != nil {
		return err
	}
	if err := a.finishReload(); err != nil {
		return err
	}
	if wasQueued {
		a.waitFor(10*time.Second, func(v rmaView) bool { return v.c1 != "queued" })
	}
	return a.settle()
}

// --- the model ---

func (a *rmaAdapter) Bash() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := rmaStatus(v.row)
	if !a.gate.pass((st == "running" || st == "needs-you") && v.c1 == "") {
		return nil
	}
	a.requested = true
	if !a.reloading {
		a.locked = a.version
	}
	if err := a.release(control.Turn{Text: block("tools.bash(" + strconv.Quote(a.cmd()) + ")")}); err != nil {
		return err
	}
	// "queued" is only a settled state while a reload is in the gap: at
	// any other time it is the split-second before the ask lands (or
	// the command is refused at once), and waiting past it is what lets
	// the real ask/end show up before this action's own state check.
	a.waitFor(10*time.Second, func(v rmaView) bool {
		if a.reloading {
			return v.c1 != ""
		}
		return v.c1 == "open" || v.c1 == "end"
	})
	return a.settle()
}

// Grant is not a spec action here: this flow never queues a call
// behind another open ask (one call per walk), only behind a reload.

// --- the person, on the ask ---

func (a *rmaAdapter) answer(text string) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	askID := v.row.Ask.ID
	ctx, cancel := actionCtx()
	defer cancel()
	code, _, err := a.post(ctx, "answer", map[string]string{"text": text, "ask": askID})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return a.settle()
	}
	a.waitFor(5*time.Second, func(v rmaView) bool { return v.c1 != "open" })
	return a.settle()
}

func (a *rmaAdapter) approval(v rmaView) bool {
	return !a.stopping && v.c1 == "open" && v.row.Ask != nil
}

func (a *rmaAdapter) ClickRun() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.approval(v)) {
		return nil
	}
	return a.answer("run")
}

func (a *rmaAdapter) ClickRefuse() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.approval(v)) {
		return nil
	}
	return a.answer("refuse")
}

// --- the Asker's timeout ---

// Timeout is the Asker's own timeout_minutes firing: dropping a file
// named for the open ask's id at BOUGH_TEST_ASK_EXPIRE_DIR is the same
// hook rules_prompt_approval.fizz uses, standing in for the real
// per-config minutes wait.
func (a *rmaAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.c1 == "open" && !a.stopping) {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.expire, v.askID), nil, 0o644); err != nil {
		return err
	}
	a.waitFor(5*time.Second, func(v rmaView) bool { return v.c1 != "open" })
	return a.settle()
}

// --- Stop ---

// Stop is the spec's single fused action: interrupting a running or
// needs-you turn always lands as "done" here (see status()).
func (a *rmaAdapter) Stop() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := rmaStatus(v.row)
	if !a.gate.pass(st == "running" || st == "needs-you") {
		return nil
	}
	a.stopping = true
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "stopped", func(r serve.Row) bool {
		return r.Status == serve.StatusStopped || r.Status == serve.StatusDone
	}); err != nil {
		return err
	}
	a.held = nil
	a.stopping = false
	a.newTurn()
	a.locked = -1
	return a.settle()
}

// --- Finish: the turn's own end, once the call has settled ---

func (a *rmaAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := rmaStatus(v.row)
	if !a.gate.pass(st == "running" && !a.stopping && v.c1 != "open" && v.c1 != "queued") {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		a.absorb()
		for len(a.held) > 0 {
			if err := a.release(control.Turn{Text: "Done."}); err != nil {
				return err
			}
		}
		ctx, cancel := actionCtx()
		row, _, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		if row.Status != serve.StatusRunning && row.Status != serve.StatusNeedsYou {
			a.newTurn()
			a.locked = -1
			return a.settle()
		}
		time.Sleep(20 * time.Millisecond)
	}
	return errors.New("finish: the turn did not end")
}

func rmaAction(name string, f func(*rmaAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) { return nil, f(m.(*rmaAdapter)) }
}

var rmaActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":       rmaAction("Prompt", (*rmaAdapter).Prompt),
	"ReloadStart":  rmaAction("ReloadStart", (*rmaAdapter).ReloadStart),
	"ReloadFinish": rmaAction("ReloadFinish", (*rmaAdapter).ReloadFinish),
	"Bash":         rmaAction("Bash", (*rmaAdapter).Bash),
	"ClickRun":     rmaAction("ClickRun", (*rmaAdapter).ClickRun),
	"ClickRefuse":  rmaAction("ClickRefuse", (*rmaAdapter).ClickRefuse),
	"Timeout":      rmaAction("Timeout", (*rmaAdapter).Timeout),
	"Stop":         rmaAction("Stop", (*rmaAdapter).Stop),
	"Finish":       rmaAction("Finish", (*rmaAdapter).Finish),
}}

func rmaOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 10, "max-parallel-runs": 0}
}

// rmaHistory reads the abstract trace off a transcript. status and c1
// alone identify every step this flow's actions produce; a config
// reload leaves nothing in history, so ReloadStart/ReloadFinish are
// never read back from a transcript — only from the live MBT run,
// which drives the adapter directly, not through a projection.
func rmaHistory(entries []history.Entry) []tracecheck.Step {
	st := func(s, c1 string) map[string]any { return map[string]any{"Session#0.status": s, "Session#0.c1": c1} }
	steps := []tracecheck.Step{{Action: "Init", State: st("idle", "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	var turns [][]history.Entry
	for i, e := range entries {
		if e.Kind == "input" {
			turns = append(turns, nil)
		}
		if len(turns) > 0 {
			turns[len(turns)-1] = append(turns[len(turns)-1], entries[i])
		}
	}
	for _, turn := range turns {
		add("Prompt", st("running", ""))
		open := ""
		closed := false
		// sawBash: this turn's one bash call already has a step, either
		// its ask (open) or its own immediate completion (no rule
		// matched, or the odd-version "forbidden" ends it at once). Its
		// real "call" entry lands either way — before the ask when it
		// asked, after when it did not — so a second one must never add
		// a second step.
		sawBash := false
		for _, e := range turn {
			if closed {
				break
			}
			switch e.Kind {
			case "ask":
				sawBash = true
				open = str(e.Data["id"])
				add("Bash", st("needs-you", "open"))
			case "call", "sub:call":
				if !sawBash && e.Data["phase"] != "start" {
					sawBash = true
					add("Bash", st("running", "end"))
				}
			case "ask/answer":
				if open == "" || str(e.Data["id"]) != open {
					continue
				}
				open = ""
				switch str(e.Data["text"]) {
				case "run":
					add("ClickRun", st("running", "end"))
				case "refuse":
					add("ClickRefuse", st("running", "end"))
				}
			case "ask/end":
				if open != "" && str(e.Data["id"]) == open {
					open = ""
					add("Timeout", st("running", "end"))
				}
			case "cancelled":
				add("Stop", st("done", ""))
				closed = true
			case "done":
				add("Finish", st("done", ""))
				closed = true
			}
		}
	}
	return steps
}

func init() { historyProjections["rules_reload_mid_approval"] = rmaHistory }

func TestRulesReloadMidApproval(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRmaAdapter(t)
	if err := runMBT(t, "rules_reload_mid_approval", a, rmaActions, rmaOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "rules_reload_mid_approval"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), rmaHistory)
	}
}

// The run above proves nothing unless a wrongly wired adapter fails
// it: this one's ReloadFinish never reports the version flip the spec
// always makes, so its very first ReloadFinish disagrees with the
// model.
func TestRulesReloadMidApprovalCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRmaAdapter(t)
	a.gateMisses = true
	if err := runMBT(t, "rules_reload_mid_approval", a, rmaActions, rmaOptions()); err == nil {
		t.Fatal("a run whose ReloadFinish never reports the version flip passed; the runner is not checking state")
	}
}
