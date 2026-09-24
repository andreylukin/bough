//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/hook_deny_effect_vs_ledger.fizz against real serves: one
// pre-code-exec hook file decides about one call, and the adapter reads
// back whether the call ran, what the model was told, and what the
// ledger (history's "hook" entries, the turn's lines the Hooks chip is
// drawn from, /api/hooks) says the hook decided.
//
// Two serves, one per engine, because the engine is a config row: code
// mode (loop.plugin=loop) answers with llm-echo, whose "CODE!" reply is
// one bash block and whose next reply echoes what the model was told;
// the engine answers with llm-control, which makes one bash call and
// records the tool result the next request carried.

const (
	hookDenyEvent = "pre-code-exec"
	hookDenyFirst = "a_first.js" // the spec's hook; sorts before later
	hookDenyLater = "z_later.js" // the pass-through hook after it

	hookDenyLoopConfig = "- id: llm\n  plugin: llm-echo\n- id: loop\n  plugin: loop\n"

	// What the call prints as sent and as a rewrite made it: code mode
	// runs echo's fixed block, the engine the command llm-control sends.
	hookDenyLoopSent   = "hi from codemode"
	hookDenyEngineSent = "RAN-SENT"
	hookDenyRewritten  = "RAN-REWRITTEN"
)

// hookDenyBodies is the first hook's file for each of the spec's RESULTS.
var hookDenyBodies = map[string]string{
	"pass":         "// pass: says nothing\nreturn;\n",
	"deny_str":     "// deny_str\nreturn {deny: \"no calls today\"};\n",
	"deny_true":    "// deny_true\nreturn {deny: true};\n",
	"deny_false":   "// deny_false\nreturn {deny: false};\n",
	"block":        "// block\nreturn {block: \"blocked today\"};\n",
	"rewrite_code": "// rewrite_code\nreturn {code: 'tools.bash(\"echo " + hookDenyRewritten + "\")'};\n",
	"rewrite_args": "// rewrite_args\nreturn {args: {command: \"echo " + hookDenyRewritten + "\"}};\n",
	"throw":        "// throw\nthrow new Error(\"the hook broke\");\n",
}

type hookDenySession struct {
	s  *servetest.Server
	id string
}

type hookDenyAdapter struct {
	t          *testing.T
	loop, eng  *servetest.Server
	gate       gate
	walk       int
	sessions   []hookDenySession
	engine     string
	hook       string
	off        bool
	phase      string
	id         string // the walk's session, once called
	readTurn   string // the engine's request that read the call's result
	callID     string
	modelSaw   string
	chip, page string

	// offNoop is the deliberate bug the wrong-adapter tests inject:
	// ToggleOff says it turned the hook off and never asks serve to.
	offNoop bool
}

func newHookDenyAdapter(t *testing.T) *hookDenyAdapter {
	files := map[string]string{
		".bough/hooks/" + hookDenyEvent + "/" + hookDenyLater: "// later: passes through\nreturn;\n",
	}
	return &hookDenyAdapter{
		t:    t,
		loop: servetest.Start(t, servetest.Options{Config: hookDenyLoopConfig, Files: files}),
		eng:  servetest.Start(t, servetest.Options{Config: controlConfig, Files: files}),
	}
}

func (a *hookDenyAdapter) serve() *servetest.Server {
	if a.engine == "engine" {
		return a.eng
	}
	return a.loop
}

func (a *hookDenyAdapter) firstPath(s *servetest.Server) string {
	return filepath.Join(s.Home, ".bough", "hooks", hookDenyEvent, hookDenyFirst)
}

// Init takes the last walk's hook away (and back on) in whichever serve
// it was in, and starts on code mode with nothing called.
func (a *hookDenyAdapter) Init() error {
	if a.off {
		if err := a.setOff(a.serve(), false); err != nil {
			return err
		}
	}
	for _, s := range []*servetest.Server{a.loop, a.eng} {
		if err := os.Remove(a.firstPath(s)); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	a.engine, a.hook, a.off, a.phase = "loop", "none", false, "setup"
	a.id, a.readTurn, a.callID, a.modelSaw, a.chip, a.page = "", "", "", "", "unseen", "unseen"
	a.gate.reset()
	return nil
}

func (a *hookDenyAdapter) Cleanup() error { return nil }

func (a *hookDenyAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Turn", Index: 0}: a}, nil
}

// GetState reads the call's outcome and the ledger from the session's
// history every step; off from off.yml as /api/hooks reports it; what
// the model read, the chip and the page from what was observed when
// those actions ran.
func (a *hookDenyAdapter) GetState() (map[string]any, error) {
	st := map[string]any{
		"engine": a.engine, "hook": a.hook, "phase": a.phase,
		"ran": "", "ledger": "", "later_fired": false,
		"model_saw": a.modelSaw, "chip": a.chip, "page": a.page,
	}
	off, err := a.hookOff()
	if err != nil {
		return nil, err
	}
	st["off"] = off
	if a.id != "" {
		entries, err := history.Read(filepath.Join(a.serve().Home, ".bough", "history", a.id+".jsonl"))
		if err != nil {
			return nil, err
		}
		o := hookDenyObserve(entries)
		st["ran"], st["ledger"], st["later_fired"] = o.ran, o.ledger, o.laterFired
	}
	return st, nil
}

// --- setup ---------------------------------------------------------------

func (a *hookDenyAdapter) UseEngine() error {
	if a.gate.pass(a.phase == "setup" && a.engine == "loop" && a.hook == "none") {
		a.engine = "engine"
	}
	return nil
}

// writeHook writes the result the walk asks for: the path walk names it
// from the step's state, the runner as the `any RESULTS` choice.
func (a *hookDenyAdapter) writeHook(result string) error {
	if !a.gate.pass(a.phase == "setup" && a.hook == "none") {
		return nil
	}
	body, ok := hookDenyBodies[result]
	if !ok {
		return fmt.Errorf("no hook body for result %q", result)
	}
	if err := os.WriteFile(a.firstPath(a.serve()), []byte(body), 0o644); err != nil {
		return err
	}
	a.hook = result
	return nil
}

func writeHookArg(m any, args []fmbt.Arg) (any, error) {
	for _, arg := range args {
		if r, ok := arg.Value.(string); ok && arg.Name == "result" {
			return nil, m.(*hookDenyAdapter).writeHook(r)
		}
	}
	return nil, fmt.Errorf("WriteHook: no string choice \"result\" in %v", args)
}

func (a *hookDenyAdapter) ToggleOff() error {
	if !a.gate.pass(a.phase == "setup" && a.hook != "none" && !a.off) {
		return nil
	}
	a.off = true
	if a.offNoop {
		return nil
	}
	return a.setOff(a.serve(), true)
}

// --- the call ------------------------------------------------------------

// Call runs the one call to the end of its turn: code mode's echo
// writes one bash block for "CODE!", the engine's first queued turn
// makes one bash call, and each then reads the result on the next
// request, which ends the turn.
func (a *hookDenyAdapter) Call() error {
	if !a.gate.pass(a.phase == "setup") {
		return nil
	}
	a.walk++
	s := a.serve()
	prompt := "CODE!"
	if a.engine == "engine" {
		name := fmt.Sprintf("hd%04d", a.walk)
		a.callID, a.readTurn = "call_"+name, name+"b"
		control.Queue(a.t, control.Dir(s.Home), name+"a", control.Turn{Mode: "ok", Calls: []control.Call{{
			ID: a.callID, Name: "bash", Args: map[string]any{"command": "echo " + hookDenyEngineSent},
		}}})
		control.Queue(a.t, control.Dir(s.Home), a.readTurn, control.Turn{Mode: "ok", Text: "read " + name})
		prompt = "call " + name
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(a.t, "work"), prompt)
	if err != nil {
		return err
	}
	a.id, a.phase = row.ID, "called"
	a.sessions = append(a.sessions, hookDenySession{s, row.ID})
	_, err = waitRow(s, row.ID, "the call's turn to end", func(r serve.Row) bool {
		return r.Status == serve.StatusDone || r.Status == serve.StatusError
	})
	return err
}

// ModelReads reads what the next request told the model: echo's reply
// repeats it in code mode; llm-control recorded it on the engine.
func (a *hookDenyAdapter) ModelReads() error {
	if !a.gate.pass(a.phase == "called") {
		return nil
	}
	a.phase = "read"
	var told string
	if a.engine == "engine" {
		rs, err := control.ToolResults(control.Dir(a.eng.Home), a.readTurn)
		if err != nil {
			return err
		}
		told = rs[a.callID]
	} else {
		entries, err := history.Read(filepath.Join(a.loop.Home, ".bough", "history", a.id+".jsonl"))
		if err != nil {
			return err
		}
		for _, e := range entries {
			if e.Kind == "assistant" {
				told = entryText(e)
			}
		}
	}
	a.modelSaw = hookDenySaw(told)
	return nil
}

// --- the control room ----------------------------------------------------

// ViewTurn reads the turn's lines as serve hands them to the page and
// applies the chip's rule (TurnHooks in app.tsx): a fire that decided,
// errored, said or returned something is a row; a denial, a block or
// an error is the red badge.
func (a *hookDenyAdapter) ViewTurn() error {
	if !a.gate.pass(a.phase == "read" && a.chip == "unseen") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.serve().GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	a.chip = hookDenyChip(lines)
	return nil
}

// ViewHooksPage is the ledger's word for the first hook's fire in this
// session, as /api/hooks serves it; "none" when it lists no such fire.
func (a *hookDenyAdapter) ViewHooksPage() error {
	if !a.gate.pass(a.phase == "read" && a.page == "unseen") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var d struct {
		Fires []serve.HookFire `json:"fires"`
	}
	if err := hookDenyCall(ctx, a.serve(), http.MethodGet, "/api/hooks", nil, &d); err != nil {
		return err
	}
	a.page = "none"
	for _, f := range d.Fires {
		if f.Session == a.id && f.Event == hookDenyEvent && f.Name == hookDenyFirst {
			a.page = ledgerWord(f.Decision, f.Error)
			break
		}
	}
	return nil
}

// --- reading the product -------------------------------------------------

type hookDenyObs struct {
	engine     string // "loop" | "engine"
	hook       string // the first hook's result as its output shows it, "" when it did not fire
	ledger     string
	laterFired bool
	ran        string
	saw        string // what the model read, from the transcript
}

// hookDenyObserve reads one Call's session: the hook entries are the
// ledger, the result entries (code mode) or the bash call's output
// (engine) say whether and what ran.
func hookDenyObserve(entries []history.Entry) hookDenyObs {
	o := hookDenyObs{engine: "loop", ledger: "none", ran: "no"}
	for _, e := range entries {
		if e.Kind == "engine" {
			o.engine = "engine" // the engine's first entry names it
		}
	}
	for _, e := range entries {
		switch e.Kind {
		case "hook":
			var f struct {
				Event    string         `json:"event"`
				Name     string         `json:"name"`
				Decision string         `json:"decision"`
				Error    string         `json:"error"`
				Output   map[string]any `json:"output"`
			}
			raw, _ := json.Marshal(e.Data)
			if json.Unmarshal(raw, &f) != nil || f.Event != hookDenyEvent {
				continue
			}
			switch f.Name {
			case hookDenyFirst:
				o.ledger = ledgerWord(f.Decision, f.Error)
				o.hook = hookResultOf(f.Output, f.Error)
			case hookDenyLater:
				o.laterFired = true
			}
		case "assistant":
			// Code mode: echo's last reply repeats what it was told.
			if o.engine != "engine" {
				o.saw = hookDenySaw(entryText(e))
			}
		case "result":
			// Code mode: what the block printed (a refusal is a result
			// too, and its "code" is the block it did not run).
			o.ran = hookDenyRan(entryText(e))
		case "call":
			// The engine: what the bash call printed is what the model
			// reads back.
			out, _ := e.Data["output"].(string)
			o.ran, o.saw = hookDenyRan(out), hookDenySaw(out)
		}
	}
	return o
}

// hookDenyRan reads which command a call's output came from.
func hookDenyRan(out string) string {
	switch {
	case strings.Contains(out, hookDenyRewritten):
		return "rewritten"
	case strings.Contains(out, hookDenyLoopSent), strings.Contains(out, hookDenyEngineSent):
		return "as_sent"
	}
	return "no"
}

// ledgerWord is the word the Hooks page and the chip show for a fire.
func ledgerWord(decision, err string) string {
	switch {
	case err != "":
		return "error"
	case decision == "":
		return "passed"
	}
	return decision
}

// hookResultOf names which of hookDenyBodies returned output.
func hookResultOf(out map[string]any, err string) string {
	if err != "" {
		return "throw"
	}
	if v, ok := out["deny"]; ok {
		switch v {
		case true:
			return "deny_true"
		case false:
			return "deny_false"
		}
		return "deny_str"
	}
	for _, k := range []string{"block", "code", "args"} {
		if _, ok := out[k]; ok {
			return map[string]string{"block": "block", "code": "rewrite_code", "args": "rewrite_args"}[k]
		}
	}
	return "pass"
}

// hookDenySaw classifies what the model was told about the call.
func hookDenySaw(told string) string {
	switch {
	case strings.Contains(told, "[hook denied:"):
		return "hook_denied"
	case strings.Contains(told, "blocked by hook:"):
		return "blocked_by_hook"
	case strings.Contains(told, hookDenyRewritten), strings.Contains(told, hookDenyLoopSent), strings.Contains(told, hookDenyEngineSent):
		return "output"
	}
	return "other: " + told
}

// hookDenyChip is TurnHooks' rule over serve's lines for the turn.
func hookDenyChip(lines []serve.Line) string {
	shown, failed := 0, false
	carried := map[string]bool{}
	for _, l := range lines {
		if l.Kind != "hook" {
			continue
		}
		dec, _ := l.Data["decision"].(string)
		errText, _ := l.Data["error"].(string)
		notice, _ := l.Data["notice"].(string)
		if dec == "" && errText == "" && notice == "" && l.Data["output"] == nil {
			continue
		}
		shown++
		carried[notice] = true
		if errText != "" || dec == "denied" || dec == "blocked" {
			failed = true
		}
	}
	// isHookLine's loose notices: a "hook <event>: ..." system line no
	// fire carries.
	for _, l := range lines {
		if l.Kind != "system" || !strings.HasPrefix(l.Text, "hook ") {
			continue
		}
		if _, n, ok := strings.Cut(l.Text, ":"); !ok || !carried[strings.TrimSpace(n)] {
			shown++
		}
	}
	switch {
	case failed:
		return "failed"
	case shown > 0:
		return "quiet"
	}
	return "absent"
}

func entryText(e history.Entry) string {
	t, _ := e.Data["text"].(string)
	return t
}

// hookOff is whether off.yml has the first hook off, as /api/hooks
// lists it; false while there is no hook file to list.
func (a *hookDenyAdapter) hookOff() (bool, error) {
	if _, err := os.Stat(a.firstPath(a.serve())); err != nil {
		return false, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var d struct {
		Hooks []serve.HookRow `json:"hooks"`
	}
	if err := hookDenyCall(ctx, a.serve(), http.MethodGet, "/api/hooks", nil, &d); err != nil {
		return false, err
	}
	for _, h := range d.Hooks {
		if h.Event == hookDenyEvent && h.Name == hookDenyFirst {
			return h.Off, nil
		}
	}
	return false, fmt.Errorf("/api/hooks does not list %s/%s", hookDenyEvent, hookDenyFirst)
}

func (a *hookDenyAdapter) setOff(s *servetest.Server, off bool) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return hookDenyCall(ctx, s, http.MethodPost, "/api/off", map[string]any{"id": "hook:" + hookDenyEvent + "/" + hookDenyFirst, "off": off}, nil)
}

// hookDenyCall is one request to serve's API as the page makes it.
func hookDenyCall(ctx context.Context, s *servetest.Server, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s: %d: %s", method, path, resp.StatusCode, strings.TrimSpace(string(raw)))
	}
	if out != nil {
		return json.Unmarshal(raw, out)
	}
	return nil
}

var hookDenyActions = map[string]map[string]fmbt.ActionFunc{"Turn": {
	"UseEngine":     action((*hookDenyAdapter).UseEngine),
	"WriteHook":     writeHookArg,
	"ToggleOff":     action((*hookDenyAdapter).ToggleOff),
	"Call":          action((*hookDenyAdapter).Call),
	"ModelReads":    action((*hookDenyAdapter).ModelReads),
	"ViewTurn":      action((*hookDenyAdapter).ViewTurn),
	"ViewHooksPage": action((*hookDenyAdapter).ViewHooksPage),
}, "": {
	// deadlock_detection is off, so fizz links a walk's resting state to
	// itself as a role-less "end", and the runner offers it everywhere;
	// taken, it matches no link and the walk stops being checked, so it
	// declines like a disabled pick (see unseenTroubleAckActions).
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*hookDenyAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func hookDenyOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// hookDenyHistory reads one Call's transcript. Setup leaves nothing in
// history but what the fires show: the engine (a native call's payload
// names its tool) and the first hook's result (its output). A hook that
// was off, or none, reads as none: the same path to the same call.
func hookDenyHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Turn#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	o := hookDenyObserve(entries)
	steps := []tracecheck.Step{{Action: "Init", State: q("phase", "setup")}}
	if o.engine == "engine" {
		steps = append(steps, tracecheck.Step{Action: "Turn#0.UseEngine", State: q("engine", "engine")})
	}
	if o.hook != "" {
		steps = append(steps, tracecheck.Step{Action: "Turn#0.WriteHook", State: q("hook", o.hook)})
	}
	steps = append(steps, tracecheck.Step{Action: "Turn#0.Call", State: q("ledger", o.ledger, "ran", o.ran, "later_fired", o.laterFired)})
	if o.saw != "" {
		steps = append(steps, tracecheck.Step{Action: "Turn#0.ModelReads", State: q("model_saw", o.saw)})
	}
	return steps
}

func init() { historyProjections["hook_deny_effect_vs_ledger"] = hookDenyHistory }

func TestHookDenyEffectVsLedger(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHookDenyAdapter(t)
	if err := runMBT(t, "hook_deny_effect_vs_ledger", a, hookDenyActions, hookDenyOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "hook_deny_effect_vs_ledger"))
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range a.sessions {
		checkHistory(t, g, sessionHistory(t, s.s.Home, s.id), hookDenyHistory)
	}
}

func TestHookDenyEffectVsLedgerCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHookDenyAdapter(t)
	a.offNoop = true
	err := runMBT(t, "hook_deny_effect_vs_ledger", a, hookDenyActions, hookDenyOptions())
	if err == nil {
		t.Fatal("a run whose ToggleOff never turns the hook off passed; the runner is not checking state")
	}
	// A runner that never started is no catch (a TMPDIR too long for
	// the plugin socket once passed this test in two seconds).
	if strings.Contains(err.Error(), "failed to listen") {
		t.Fatalf("the runner did not start: %v", err)
	}
	t.Log(err)
}

// walkHookDenyPaths walks the generator's paths over the checked-in
// graph, comparing the adapter's state with the spec's after every
// step. WriteHook takes its result from the step's state: the walk
// names the branch of `any RESULTS` it took. With firstOnly it stops at
// the first mismatch.
func walkHookDenyPaths(t *testing.T, a *hookDenyAdapter, cover tracecheck.Cover, firstOnly bool) []string {
	t.Helper()
	b, err := pathsJSONCover("hook_deny_effect_vs_ledger", cover)
	if err != nil {
		t.Fatal(err)
	}
	var out struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	if len(out.Paths) == 0 {
		t.Fatal("no paths over testdata/hook_deny_effect_vs_ledger")
	}
	t.Logf("%d paths (%s)", len(out.Paths), cover)
	var bad []string
	for i, p := range out.Paths {
	steps:
		for j, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Turn#0.")
			switch {
			case j == 0:
				err = a.Init()
			case step.Action == "end":
				// The resting state's self-link: nothing happens, and the
				// state is checked again below.
			case name == "WriteHook":
				r, _ := step.State["Turn#0.hook"].(string)
				err = a.writeHook(r)
			default:
				f, ok := hookDenyActions["Turn"][name]
				if !ok {
					t.Fatalf("path %d: no adapter action for %s", i, step.Action)
				}
				_, err = f(a, nil)
			}
			if err == nil && a.gate.off {
				err = fmt.Errorf("the adapter found %s disabled", step.Action)
			}
			var got map[string]any
			if err == nil {
				got, err = a.GetState()
			}
			if err != nil {
				bad = append(bad, fmt.Sprintf("path %d step %d (%s): %v", i, j, step.Action, err))
				break
			}
			for k, want := range step.State {
				field, ok := strings.CutPrefix(k, "Turn#0.")
				if ok && got[field] != want {
					bad = append(bad, fmt.Sprintf("path %d step %d (%s) engine=%v hook=%v off=%v: %s is %v, the spec says %v%s",
						i, j, step.Action, got["engine"], got["hook"], got["off"], field, got[field], want, a.transcript()))
					break steps
				}
			}
		}
		if firstOnly && len(bad) > 0 {
			break
		}
	}
	return bad
}

// transcript is the walk's session, one line per entry, for a mismatch
// to show what the product recorded.
func (a *hookDenyAdapter) transcript() string {
	if a.id == "" {
		return ""
	}
	entries, err := history.Read(filepath.Join(a.serve().Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return "\n\t(no transcript: " + err.Error() + ")"
	}
	var b strings.Builder
	for _, e := range entries {
		raw, _ := json.Marshal(e.Data)
		if len(raw) > 300 {
			raw = append(raw[:300], "..."...)
		}
		fmt.Fprintf(&b, "\n\t%s %s", e.Kind, raw)
	}
	return b.String()
}

func hookDenyTestdata() string {
	return filepath.Join(filepath.Dir(specPath("hook_deny_effect_vs_ledger")), "..", "testdata", "hook_deny_effect_vs_ledger")
}

// TestHookDenyEffectVsLedgerPaths needs no fizz tools: the walks come
// from the checked-in graph, and every transcript they wrote is then
// replayed on it.
func TestHookDenyEffectVsLedgerPaths(t *testing.T) {
	t.Parallel()
	a := newHookDenyAdapter(t)
	for _, b := range walkHookDenyPaths(t, a, envCover(), false) {
		t.Error(b)
	}
	if len(a.sessions) == 0 {
		t.Fatal("no path made the call; the ledger went unchecked")
	}
	g, err := tracecheck.Load(hookDenyTestdata())
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range a.sessions {
		checkHistory(t, g, sessionHistory(t, s.s.Home, s.id), hookDenyHistory)
	}
}

// The wrong ToggleOff shows only on the transitions from an off hook to
// the call, so this walks every transition, and stops at the first
// mismatch.
func TestHookDenyEffectVsLedgerPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newHookDenyAdapter(t)
	a.offNoop = true
	bad := walkHookDenyPaths(t, a, tracecheck.CoverTransitions, true)
	if len(bad) == 0 {
		t.Fatal("a path walk whose ToggleOff never turns the hook off passed; it is not checking state")
	}
	// Caught for the injected reason, not some other breakage.
	if !strings.Contains(bad[0], "ToggleOff") && !strings.Contains(bad[0], "off=true") {
		t.Fatalf("the walk failed, but not on the hook left on: %s", bad[0])
	}
	t.Log(bad[0])
}
