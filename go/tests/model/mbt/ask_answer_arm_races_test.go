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
	"slices"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/linegate"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/ask_answer_arm_races.fizz: the races between a question the
// child asked, serve arming it when its pump reads the event, two
// answerers and a /prompt, against a real serve. The pipes between
// serve and the child are stepped one line at a time through
// internal/linegate (BOUGH_TEST_LINE_GATE, a folder in each walk's own
// cwd): ArmFromEvent lets serve's pump read the next ask or ask-end
// line, ReadLine lets the child route the next stdin line. The model's
// asks are llm-control call turns, a timeout fires on cue through
// BOUGH_TEST_ASK_EXPIRE_DIR, and the secret lands in a file keychain.
//
// Which question is which is read off history: A is the plain ask, B
// the secret. Answers and messages carry markers, so where a line ended
// up (an answer, a steer, the keychain) says which POST wrote it.

const aaarGate = ".gate" // relative: each child gates in its own cwd

type aaarAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	expire   string
	keychain string
	gate     gate

	id, cwd    string
	turn, n    int
	held, next string

	// The page's side, which is the spec's own: the POST each tab has in
	// flight (the question it showed and that question's id), the one
	// /prompt, and every line serve accepted for the child's stdin, in
	// order ("start" first). wrote and the three write flags are read
	// off serve's answers to the POSTs and the arm probed around them.
	tab0, tab0ID, tab1, tab1ID string
	prompt, prompted           bool
	lines                      []string
	wrote                      []string
	dupWrite, unarmed, wrongDA bool

	ids []string
	did map[string]int

	// tab1NoID is the deliberate bug the wrong-adapter test injects: the
	// second answerer names no question, so serve answers whatever is
	// armed.
	tab1NoID bool
}

func newAaarAdapter(t *testing.T) *aaarAdapter {
	expire, keychain := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env: []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire, "BOUGH_TEST_KEYCHAIN_DIR=" + keychain,
			linegate.Env + "=" + aaarGate},
	})
	return &aaarAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, keychain: keychain, did: map[string]int{}}
}

func (a *aaarAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("r%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

func (a *aaarAdapter) takeNext() error {
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// gateFile is one of the linegate files of the walk's child.
func (a *aaarAdapter) gateFile(side string, n int, ext string) string {
	return filepath.Join(a.cwd, aaarGate, fmt.Sprintf("%s.%d.%s", side, n, ext))
}

// passed is how many lines of side have been handled.
func (a *aaarAdapter) passed(side string) int {
	n := 0
	for {
		if _, err := os.Stat(a.gateFile(side, n, "done")); err != nil {
			return n
		}
		n++
	}
}

// let lets line n of side through and returns its outcome.
func (a *aaarAdapter) let(side string, n int) (string, error) {
	if err := waitFor(fmt.Sprintf("%s line %d to arrive", side, n), func() bool {
		_, err := os.Stat(a.gateFile(side, n, "held"))
		return err == nil
	}); err != nil {
		return "", err
	}
	if err := os.WriteFile(a.gateFile(side, n, "go"), nil, 0o644); err != nil {
		return "", err
	}
	var out []byte
	err := waitFor(fmt.Sprintf("%s line %d to be handled", side, n), func() bool {
		b, err := os.ReadFile(a.gateFile(side, n, "done"))
		out = b
		return err == nil
	})
	return string(out), err
}

func waitFor(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not after %s", what, actionTimeout)
		}
		time.Sleep(5 * time.Millisecond)
	}
	return nil
}

func (a *aaarAdapter) Init() error {
	os.Remove(filepath.Join(a.keychain, keychainFile()))
	a.queueNext()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", len(a.ids)))
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.ids = append(a.ids, row.ID)
	a.tab0, a.tab0ID, a.tab1, a.tab1ID = "", "", "", ""
	a.prompt, a.prompted = false, false
	a.lines, a.wrote = []string{"start"}, []string{}
	a.dupWrite, a.unarmed, a.wrongDA = false, false, false
	a.gate.reset()
	if _, err := a.let("in", 0); err != nil {
		return err
	}
	if err := a.takeNext(); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// Cleanup opens both gates so the child's lines drain, then kills it
// and takes back the turn it had queued.
func (a *aaarAdapter) Cleanup() error {
	if a.cwd != "" {
		os.WriteFile(filepath.Join(a.cwd, aaarGate, "open"), nil, 0o644)
	}
	if a.id != "" {
		k := &askAnswerAdapter{s: a.s, id: a.id, cwd: a.cwd}
		if err := k.kill(); err != nil {
			return err
		}
	}
	a.held = ""
	if a.next != "" {
		os.Remove(filepath.Join(a.dir, a.next+".json"))
		a.next = ""
	}
	ents, _ := os.ReadDir(a.expire)
	for _, e := range ents {
		os.Remove(filepath.Join(a.expire, e.Name()))
	}
	return nil
}

func (a *aaarAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// aaarView is what one step reads off the server.
type aaarView struct {
	row     serve.Row
	entries []history.Entry
	status  string
	asked   int
	open    string // the child's open question: "", "A", "B"
	openID  string
	out     []string // the child's ask and ask-end lines serve's pump has not read
	armed   string
	pipe    []string // stdin lines serve wrote that the child has not routed
}

func aaarQuestion(e history.Entry) string {
	if s, _ := e.Data["secret"].(bool); s {
		return "B"
	}
	return "A"
}

func (a *aaarAdapter) view() (aaarView, error) {
	var v aaarView
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	// needs-you is the row showing the child's open question; the spec's
	// status is the turn's.
	switch row.Status {
	case serve.StatusRunning, serve.StatusNeedsYou:
		v.status = "running"
	default:
		v.status = string(row.Status)
	}
	v.entries, err = history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return v, err
	}
	// The child's question and the lines it printed for serve to arm and
	// disarm on, in order: each ask, and each ask call's end.
	var lines []string
	for _, e := range v.entries {
		switch e.Kind {
		case "ask":
			v.asked++
			v.open, v.openID = aaarQuestion(e), str(e.Data["id"])
			lines = append(lines, v.open)
		case "ask/answer":
			if str(e.Data["id"]) == v.openID {
				v.open, v.openID = "", ""
			}
		case "call":
			if t := str(e.Data["tool"]); (t == "ask" || t == "secret") && e.Data["phase"] != "start" {
				q := "A"
				if t == "secret" {
					q = "B"
				}
				lines = append(lines, "end"+q)
				if v.open == q {
					v.open, v.openID = "", ""
				}
			}
		}
	}
	n := a.passed("out")
	if n > len(lines) {
		return v, fmt.Errorf("serve read %d ask lines, history has %d", n, len(lines))
	}
	v.out = append([]string{}, lines[n:]...)
	read := ""
	for _, l := range lines[:n] {
		if l == "A" || l == "B" {
			read = l
		}
	}
	if n := a.passed("in"); n <= len(a.lines) {
		v.pipe = append([]string{}, a.lines[n:]...)
	} else {
		return v, fmt.Errorf("the child routed %d stdin lines, serve accepted %d", n, len(a.lines))
	}
	v.armed, err = a.armed(read)
	return v, err
}

// armed probes serve's arm without touching it: an answer naming no
// real question is refused as expired while one is armed, and as no
// pending ask otherwise. Asks never overlap, so the armed one is the
// last one serve's pump read.
func (a *aaarAdapter) armed(read string) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	switch {
	case err != nil:
		return "", err
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
		return "", nil
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
	default:
		return "", fmt.Errorf("arm probe: %d %s", code, msg)
	}
	if read == "" {
		return "armed before any ask was read", nil
	}
	return read, nil
}

func (a *aaarAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
	k := &askAnswerAdapter{s: a.s, id: a.id}
	return k.post(ctx, verb, body)
}

func (a *aaarAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	var eaten, asPrompt, leak bool
	for i, e := range v.entries {
		switch e.Kind {
		case "ask/answer":
			if strings.HasPrefix(str(e.Data["text"]), "msg-") {
				eaten = true
			}
		case "input":
			if t := str(e.Data["text"]); i > 0 && (strings.Contains(t, "ans-A-") || strings.Contains(t, "S3CR3T-")) {
				asPrompt = true
			}
		}
		if b, _ := json.Marshal(e.Data); strings.Contains(string(b), "S3CR3T-") {
			leak = true
		}
	}
	if b, err := os.ReadFile(filepath.Join(a.keychain, keychainFile())); err == nil && strings.HasPrefix(string(b), "msg-") {
		eaten = true
	}
	return map[string]any{
		"status":         v.status,
		"asked":          v.asked,
		"open":           v.open,
		"out":            v.out,
		"armed":          v.armed,
		"pipe":           v.pipe,
		"tab0":           a.tab0,
		"tab1":           a.tab1,
		"prompt":         a.prompt,
		"prompted":       a.prompted,
		"wrote":          append([]string{}, a.wrote...),
		"dupWrite":       a.dupWrite,
		"unarmedWrite":   a.unarmed,
		"wrongDisarm":    a.wrongDA,
		"promptEaten":    eaten,
		"answerAsPrompt": asPrompt,
		"secretLeak":     leak,
	}, nil
}

// Each action asks the gate with the spec's require first.

func (a *aaarAdapter) ask(tool string, args map[string]any, asked int) error {
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: tool, Args: args})
	a.held = ""
	return a.waitHistory("the ask", func(v aaarView) bool { return v.asked == asked && v.open != "" })
}

// waitHistory polls the view until ok holds.
func (a *aaarAdapter) waitHistory(what string, ok func(aaarView) bool) error {
	var last aaarView
	err := waitFor(what, func() bool {
		v, err := a.view()
		last = v
		return err == nil && ok(v)
	})
	if err != nil {
		return fmt.Errorf("%w (open %q, asked %d, out %v)", err, last.open, last.asked, last.out)
	}
	return nil
}

// ended holds once the open question's call has ended in history: its
// end line is one more than before. The question closes earlier, on
// its answer entry, and with a steer pending the model's next request
// can come before the end is recorded.
func (a *aaarAdapter) ended(before aaarView) func(aaarView) bool {
	n := a.passed("out") + len(before.out)
	return func(w aaarView) bool { return w.open == "" && a.passed("out")+len(w.out) > n }
}

func (a *aaarAdapter) AskA() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.status == "running" && v.open == "" && v.asked == 0) {
		return nil
	}
	if err := a.ask("ask", map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}}, 1); err != nil {
		return err
	}
	// The child routes stdin by the question it has open from the moment
	// it prints the event: wait for serve's pump to be holding it.
	return waitFor("A's event on stdout", func() bool {
		_, err := os.Stat(a.gateFile("out", 0, "held"))
		return err == nil
	})
}

func (a *aaarAdapter) AskB() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.status == "running" && v.open == "" && v.asked == 1) {
		return nil
	}
	return a.ask("secret", map[string]any{"name": askSecret, "question": "the API token", "project": askProject}, 2)
}

func (a *aaarAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.open != "") {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.expire, v.openID), nil, 0o644); err != nil {
		return err
	}
	if err := a.waitHistory("the ask's call to end", a.ended(v)); err != nil {
		return err
	}
	return a.takeNext()
}

// Finish lets the turn end; a steer that landed meanwhile makes one
// more request, which is released too.
func (a *aaarAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.status == "running" && v.open == "" && v.asked == 2) {
		return nil
	}
	return a.finishTurn()
}

func (a *aaarAdapter) finishTurn() error {
	for {
		if a.held == "" {
			return errors.New("finish: no model request is held")
		}
		control.Release(a.t, a.dir, a.held)
		a.held = ""
		deadline := time.Now().Add(actionTimeout)
		for a.held == "" {
			if time.Now().After(deadline) {
				return errors.New("finish: the turn neither ended nor asked again")
			}
			if _, err := os.Stat(filepath.Join(a.dir, a.next+".taken")); err == nil {
				a.held = a.next
				a.queueNext()
				break
			}
			ctx, cancel := actionCtx()
			row, _, err := a.s.GetSession(ctx, a.id)
			cancel()
			if err != nil {
				return err
			}
			if row.Status == serve.StatusDone {
				return nil
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
}

func (a *aaarAdapter) ArmFromEvent() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(len(v.out) > 0) {
		return nil
	}
	_, err = a.let("out", a.passed("out"))
	return err
}

// ReadLine lets the child route its next stdin line, then waits for
// what that route does: an answer returns the ask (its call ends and
// the model is asked again), a steer lands in history, a prompt with no
// turn open is a turn of its own, which is let finish.
func (a *aaarAdapter) ReadLine() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(len(v.pipe) > 0) {
		return nil
	}
	n := a.passed("in")
	route, err := a.let("in", n)
	if err != nil {
		return err
	}
	switch route {
	case "answer":
		if err := a.waitHistory("the answered ask's call to end", a.ended(v)); err != nil {
			return err
		}
		return a.takeNext()
	case "steer":
		return a.waitEntries(len(v.entries), "the steer", func(e history.Entry) bool { return e.Kind == "input" })
	case "input":
		if v.status == "running" {
			// Queued behind the running turn: Finish runs it.
			return nil
		}
		if err := a.takeNext(); err != nil {
			return err
		}
		return a.finishTurn()
	case "drop":
		return nil
	}
	return fmt.Errorf("stdin line %d went to %q", n, route)
}

func (a *aaarAdapter) waitEntries(n int, what string, ok func(history.Entry) bool) error {
	k := &askAnswerAdapter{s: a.s, id: a.id}
	return k.waitEntries(n, what, ok)
}

func (a *aaarAdapter) ClickTab0() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.tab0 == "" && v.open != "") {
		return nil
	}
	a.tab0, a.tab0ID = v.open, v.openID
	return nil
}

func (a *aaarAdapter) ClickTab1() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.tab1 == "" && v.open == "A") {
		return nil
	}
	a.tab1, a.tab1ID = v.open, v.openID
	return nil
}

func (a *aaarAdapter) DeliverTab0() error {
	if !a.gate.pass(a.tab0 != "") {
		return nil
	}
	q, id := a.tab0, a.tab0ID
	a.tab0, a.tab0ID = "", ""
	return a.deliverAnswer(q, id)
}

func (a *aaarAdapter) DeliverTab1() error {
	if !a.gate.pass(a.tab1 != "") {
		return nil
	}
	q, id := a.tab1, a.tab1ID
	a.tab1, a.tab1ID = "", ""
	if a.tab1NoID {
		id = ""
	}
	return a.deliverAnswer(q, id)
}

// deliverAnswer is serve handling one tab's POST /answer for question q.
func (a *aaarAdapter) deliverAnswer(q, id string) error {
	before, err := a.view()
	if err != nil {
		return err
	}
	a.n++
	text := fmt.Sprintf("ans-A-%d", a.n)
	if q == "B" {
		text = fmt.Sprintf("S3CR3T-%d", a.n)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": text, "ask": id})
	switch {
	case err != nil:
		return err
	case code == http.StatusConflict:
		return nil // refused: nothing written
	case code != http.StatusOK:
		return fmt.Errorf("answer %s: %d %s", q, code, msg)
	}
	if before.armed != q {
		a.unarmed = true
	}
	if slices.Contains(a.wrote, q) {
		a.dupWrite = true
	}
	a.wrote = append(a.wrote, q)
	a.lines = append(a.lines, "ans"+q)
	after, err := a.view()
	if err != nil {
		return err
	}
	if before.armed != q && before.armed != "" && after.armed == "" {
		a.wrongDA = true
	}
	return nil
}

func (a *aaarAdapter) SendPrompt() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.prompted && v.status == "running") {
		return nil
	}
	a.prompted, a.prompt = true, true
	return nil
}

func (a *aaarAdapter) DeliverPrompt() error {
	if !a.gate.pass(a.prompt) {
		return nil
	}
	a.prompt = false
	a.n++
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "prompt", map[string]string{"text": fmt.Sprintf("msg-%d", a.n)})
	switch {
	case err != nil:
		return err
	case code == http.StatusConflict:
		return nil // an ask is armed: refused
	case code != http.StatusOK:
		return fmt.Errorf("prompt: %d %s", code, msg)
	}
	a.lines = append(a.lines, "prompt")
	return nil
}

func aaarCounted(name string, f func(*aaarAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*aaarAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var aaarActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AskA":          aaarCounted("AskA", (*aaarAdapter).AskA),
	"AskB":          aaarCounted("AskB", (*aaarAdapter).AskB),
	"Timeout":       aaarCounted("Timeout", (*aaarAdapter).Timeout),
	"Finish":        aaarCounted("Finish", (*aaarAdapter).Finish),
	"ArmFromEvent":  aaarCounted("ArmFromEvent", (*aaarAdapter).ArmFromEvent),
	"ReadLine":      aaarCounted("ReadLine", (*aaarAdapter).ReadLine),
	"ClickTab0":     aaarCounted("ClickTab0", (*aaarAdapter).ClickTab0),
	"ClickTab1":     aaarCounted("ClickTab1", (*aaarAdapter).ClickTab1),
	"DeliverTab0":   aaarCounted("DeliverTab0", (*aaarAdapter).DeliverTab0),
	"DeliverTab1":   aaarCounted("DeliverTab1", (*aaarAdapter).DeliverTab1),
	"SendPrompt":    aaarCounted("SendPrompt", (*aaarAdapter).SendPrompt),
	"DeliverPrompt": aaarCounted("DeliverPrompt", (*aaarAdapter).DeliverPrompt),
}, "": {
	// A quiet session with nothing enabled links to itself as "end".
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

// aaarHistory reads the abstract trace off a transcript. Arms, clicks
// and POSTs leave nothing in history, so the projection takes the one
// path every transcript also is: serve arms a question only right
// before it is answered (so a prompt always finds nothing armed), an
// answered question was clicked, delivered and read by the child, an
// ask call that ended unanswered timed out, and the one message a walk
// may send was sent at the start and read where its input landed. The
// check is on status, asked and open.
func aaarHistory(entries []history.Entry) []tracecheck.Step {
	st := func(status string, asked int, open string) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.asked": asked, "Session#0.open": open}
	}
	status, asked, open, openID, answered := "running", 0, "", "", false
	var out []string
	armed := ""
	steps := []tracecheck.Step{{Action: "Init", State: st(status, asked, open)}}
	add := func(action string) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: st(status, asked, open)})
	}
	inputs := 0
	for _, e := range entries {
		if e.Kind == "input" {
			inputs++
		}
	}
	if inputs > 1 {
		// Sent at the start, while the turn was surely running.
		add("SendPrompt")
	}
	first := true
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if first {
				first = false
				continue
			}
			add("DeliverPrompt")
			add("ReadLine")
		case "ask":
			asked++
			open, openID, answered = aaarQuestion(e), str(e.Data["id"]), false
			out = append(out, open)
			if open == "A" {
				add("AskA")
			} else {
				add("AskB")
			}
		case "ask/answer":
			if open == "" || str(e.Data["id"]) != openID {
				continue
			}
			for armed != open && len(out) > 0 {
				ev := out[0]
				out = out[1:]
				if ev == "A" || ev == "B" {
					armed = ev
				} else if ev == "end"+armed {
					armed = ""
				}
				add("ArmFromEvent")
			}
			add("ClickTab0")
			add("DeliverTab0")
			armed = ""
			q := open
			open, answered = "", true
			out = append(out, "end"+q)
			add("ReadLine")
		case "call":
			if t := str(e.Data["tool"]); (t == "ask" || t == "secret") && e.Data["phase"] != "start" && open != "" && !answered {
				out = append(out, "end"+open)
				open = ""
				add("Timeout")
			}
		case "done":
			if status == "running" && asked == 2 && open == "" {
				status = "done"
				add("Finish")
			}
		}
	}
	return steps
}

func init() { historyProjections["ask_answer_arm_races"] = aaarHistory }

func TestAskAnswerArmRaces(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAaarAdapter(t)
	opts := map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
	if err := runMBT(t, "ask_answer_arm_races", a, aaarActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g := loadAaarGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), aaarHistory)
	}
}

func aaarPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("ask_answer_arm_races", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range f.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// walkAaarPath drives one generated path and compares the whole state
// after every step; the first difference ends the path.
func walkAaarPath(a *aaarAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		gb, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gb, &g)
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(g[field]) != fmt.Sprint(v) {
				return fmt.Errorf("step %d (%s): %s = %v, want %v\n  got  %s", i, path[i].Action, field, g[field], v, gb)
			}
		}
		return nil
	}
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0, path[0].State); err != nil {
		return err
	}
	for i := 1; i < len(path); i++ {
		if path[i].Action == "end" {
			continue
		}
		name := strings.TrimPrefix(path[i].Action, "Session#0.")
		fn, ok := aaarActions["Session"][name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		if _, err := fn(a, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i, path[i].State); err != nil {
			return err
		}
	}
	return nil
}

func aaarActs(p []tracecheck.Step) []string {
	var acts []string
	for _, st := range p[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Session#0."))
	}
	return acts
}

// TestAskAnswerArmRacesPaths walks every generated path (every settled
// state; every link under MODEL_COVER=transitions). Four serves share
// the walks.
func TestAskAnswerArmRacesPaths(t *testing.T) {
	t.Parallel()
	paths := aaarPaths(t, envCover())
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newAaarAdapter(t)
			failed := 0
			for i := s; i < len(paths); i += shards {
				if err := walkAaarPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, aaarActs(paths[i]), err)
					if failed++; failed == 5 {
						t.Log("five paths failed; not walking the rest")
						break
					}
				}
			}
			t.Logf("steps taken: %v", a.did)
			g := loadAaarGraph(t)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), aaarHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrongly wired adapter fails it. The
// bug (the second tab names no question) shows only on DeliverTab1
// while B is armed, so the walks come from every link, and only those
// that take that one are walked.
func TestAskAnswerArmRacesPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	var picked [][]tracecheck.Step
	for _, p := range aaarPaths(t, tracecheck.CoverTransitions) {
		for i := 1; i < len(p); i++ {
			if p[i].Action == "Session#0.DeliverTab1" && p[i-1].State["Session#0.armed"] == "B" {
				picked = append(picked, p[:i+1])
				break
			}
		}
	}
	if len(picked) == 0 {
		t.Fatal("no walk delivers the second tab's answer while B is armed")
	}
	a := newAaarAdapter(t)
	a.tab1NoID = true
	for _, p := range picked {
		if err := walkAaarPath(a, p); err != nil {
			t.Logf("path %v failed as it must: %v", aaarActs(p), err)
			return
		}
	}
	t.Fatalf("all %d walks passed with the second tab naming no question; the walk is not checking state", len(picked))
}

func loadAaarGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("ask_answer_arm_races")), "..", "testdata", "ask_answer_arm_races"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
