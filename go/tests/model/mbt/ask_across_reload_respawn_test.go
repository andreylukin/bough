//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/ask_across_reload_respawn.fizz: an open tools.ask across a ui
// or ask row remount and across the child's exit and respawn, driven
// through a real serve. The model's asks are llm-control call turns.
// The remounts are real config hot reloads of $HOME/.bough/bough.yml:
// ReloadUi disables the ui row (the gap stays open until RemountUi
// enables it again), ReloadAsk changes the ask row's config (the ui row
// remounts with it, as its dependent) and then disables the ui row, so
// the gap the spec leaves open after it is open here too.

// askRRConfig is the HOME overlay: llm-control, and the two rows the
// walk reloads, spelled out so a write that changes one is a diff.
func askRRConfig(timeout int, uiUp bool) string {
	c := controlConfig + fmt.Sprintf("- id: ask\n  plugin: ask\n  config:\n    timeout_minutes: %d\n- id: ui\n  plugin: ui\n", timeout)
	if !uiUp {
		c += "  disabled: true\n"
	}
	return c
}

// askRRAdapter plays the page (draft, and the POSTs it sends) and the
// person's config edits. What it cannot read off the server it tracks:
// which Asker is current (gen, seq, reloads, respawns), whether the ui
// row is mounted, and a line delivered into the gap (held).
type askRRAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	expire   string // BOUGH_TEST_ASK_EXPIRE_DIR
	keychain string // BOUGH_TEST_KEYCHAIN_DIR
	gate     gate

	id   string
	cwd  string
	turn int
	n    int    // answer markers, unique across walks
	held string // the model request held in flight
	next string // the block turn queued for the next request

	gen, seq, reloads, respawns int
	uiUp                        bool
	timeout                     int // the ask row's timeout_minutes, the config's
	written                     int // config writes this session's live child reloaded
	specIDs                     []string
	genOf                       []int

	line, lineSrc, lineProd string // held: "ans"/"sec", its question, the product ask id
	draft, draftID          string // the composer draft's question and its ask (spec id)
	draftProd               string // draftAsk as the page keeps it: the product id

	ids []string
	did map[string]int

	// reloadUiNoop is the deliberate bug the wrong-adapter test injects:
	// ReloadUi writes nothing, so an answer "in the gap" is routed at once.
	reloadUiNoop bool
}

func newAskRRAdapter(t *testing.T) *askRRAdapter {
	expire, keychain := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: askRRConfig(10, true),
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire, "BOUGH_TEST_KEYCHAIN_DIR=" + keychain},
	})
	return &askRRAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, keychain: keychain, did: map[string]int{}, timeout: 10}
}

func (a *askRRAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("r%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

func (a *askRRAdapter) takeNext() error {
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// writeConfig replaces the overlay (rename, so the watcher never reads
// half a file) and, while the session's child is alive, waits for its
// "bough: reloaded" line: serve relays the child's stderr as events.
func (a *askRRAdapter) writeConfig(timeout int, uiUp bool, live bool) error {
	path := filepath.Join(a.s.Home, ".bough", "bough.yml")
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(askRRConfig(timeout, uiUp)), 0o644); err != nil {
		return err
	}
	if err := os.Rename(tmp, path); err != nil {
		return err
	}
	a.timeout = timeout
	if !live {
		return nil
	}
	a.written++
	ctx, cancel := actionCtx()
	defer cancel()
	st, err := a.s.Events(ctx, a.id)
	if err != nil {
		return err
	}
	defer st.Close()
	seen := 0
	_, err = st.WaitFor(ctx, func(ev serve.Event) bool {
		if strings.Contains(ev.Text, "bough: reloaded") {
			seen++
		}
		return seen >= a.written
	})
	if err != nil {
		return fmt.Errorf("waiting for reload %d: %w", a.written, err)
	}
	return nil
}

func (a *askRRAdapter) Init() error {
	for _, f := range []string{"TOKEN_Q1", "TOKEN_Q2"} {
		os.Remove(filepath.Join(a.keychain, "bough%"+askProject+"%"+f))
	}
	if err := a.writeConfig(10, true, false); err != nil {
		return err
	}
	a.queueNext()
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", len(a.ids)))
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.gen, a.seq, a.reloads, a.respawns, a.uiUp, a.written = 1, 0, 0, 0, true, 0
	a.specIDs, a.genOf = nil, nil
	a.line, a.lineSrc, a.lineProd = "", "", ""
	a.draft, a.draftID, a.draftProd = "", "", ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	if err := a.takeNext(); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func (a *askRRAdapter) Cleanup() error {
	if err := a.kill(); err != nil {
		return err
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

// kill SIGKILLs the session's child, found by the walk's own cwd.
func (a *askRRAdapter) kill() error {
	out, _ := exec.Command("lsof", "-a", "-d", "cwd", "-c", "bough", "-Fpn").Output()
	pid := 0
	for _, l := range strings.Split(string(out), "\n") {
		switch {
		case strings.HasPrefix(l, "p"):
			pid, _ = strconv.Atoi(l[1:])
		case strings.HasPrefix(l, "n") && l[1:] == a.cwd && pid > 0:
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	_, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *askRRAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// rrAsk is one ask entry of the session's history, by ordinal.
type rrAsk struct {
	prod   string // the product's id
	secret bool
}

// rrView is what one step reads off the server.
type rrView struct {
	row     serve.Row
	entries []history.Entry
	asks    []rrAsk
	open    int // ordinal of the ask the child is blocked on, -1 none
	rowK    int // ordinal of the ask the row shows, -1 none
	armed   bool
}

func (a *askRRAdapter) view() (rrView, error) {
	v := rrView{open: -1, rowK: -1}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	v.entries, err = history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return v, err
	}
	// The blocked call, from history: an ask entry opens it; its answer,
	// the native call's recorded end, a block's result or the turn's
	// close ends it. A child that is gone has nothing open.
	for _, e := range v.entries {
		switch e.Kind {
		case "ask":
			s, _ := e.Data["secret"].(bool)
			v.asks = append(v.asks, rrAsk{prod: str(e.Data["id"]), secret: s})
			v.open = len(v.asks) - 1
		case "ask/answer":
			if v.open >= 0 && str(e.Data["id"]) == v.asks[v.open].prod {
				v.open = -1
			}
		case "call":
			if t := str(e.Data["tool"]); t == "ask" || t == "secret" {
				v.open = -1
			}
		case "result", "done", "cancelled":
			v.open = -1
		}
	}
	if !row.Live {
		v.open = -1
	}
	if row.Ask != nil {
		for k := len(v.asks) - 1; k >= 0; k-- {
			if v.asks[k].prod == row.Ask.ID {
				v.rowK = k
				break
			}
		}
	}
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	switch {
	case err != nil:
		return v, err
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
		v.armed = true
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
	default:
		return v, fmt.Errorf("arm probe: %d %s", code, msg)
	}
	return v, nil
}

// specID is the spec's name for the k-th ask of the session.
func (a *askRRAdapter) specID(k int) string {
	if k < 0 {
		return ""
	}
	if k < len(a.specIDs) {
		return a.specIDs[k]
	}
	return fmt.Sprintf("unexpected-ask-%d", k+1)
}

func qOf(k int) string {
	if k < 0 {
		return ""
	}
	return fmt.Sprintf("q%d", k+1)
}

// GetState is the Session role's state. The ghosts are read off history
// and the keychain, never off the adapter's intent.
func (a *askRRAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	used := []any{}
	seen := map[string]bool{}
	dup := false
	for k, q := range v.asks {
		used = append(used, a.specID(k))
		if seen[q.prod] {
			dup = true
		}
		seen[q.prod] = true
	}
	var asPrompt, secretIn, wrong bool
	latest := map[string]int{} // product id -> ordinal, latest wins
	k := -1
	for _, e := range v.entries {
		switch e.Kind {
		case "ask":
			k++
			latest[str(e.Data["id"])] = k
		case "ask/answer":
			text := str(e.Data["text"])
			if q, ok := latest[str(e.Data["id"])]; ok && strings.HasPrefix(text, "ans-") && !strings.HasPrefix(text, "ans-"+qOf(q)+"-") {
				wrong = true
			}
		case "input":
			text := str(e.Data["text"])
			if strings.Contains(text, "ans-q") || strings.Contains(text, "S3CR3T") {
				asPrompt = true
			}
			if strings.Contains(text, "S3CR3T") {
				secretIn = true
			}
		}
	}
	for i := 1; i <= 2; i++ {
		b, err := os.ReadFile(filepath.Join(a.keychain, fmt.Sprintf("bough%%%s%%TOKEN_Q%d", askProject, i)))
		if err == nil && !strings.Contains(string(b), fmt.Sprintf("-q%d-", i)) {
			wrong = true
		}
	}
	armed := ""
	if v.armed && len(v.asks) > 0 {
		armed = a.specID(len(v.asks) - 1)
	}
	openGen := 0
	if v.open >= 0 && v.open < len(a.genOf) {
		openGen = a.genOf[v.open]
	}
	rowSecret := v.rowK >= 0 && v.row.Ask.Secret
	return map[string]any{
		"alive":          v.row.Live,
		"gen":            a.gen,
		"seq":            a.seq,
		"reloads":        a.reloads,
		"respawns":       a.respawns,
		"uiUp":           a.uiUp,
		"asked":          len(v.asks),
		"open":           qOf(v.open),
		"openId":         a.specID(v.open),
		"openGen":        openGen,
		"hlAsk":          a.specID(v.open), // the router's id is the open ask's in every state the spec reaches
		"held":           a.line,
		"heldSrc":        a.lineSrc,
		"armed":          armed,
		"row":            a.specID(v.rowK),
		"rowQ":           qOf(v.rowK),
		"rowSecret":      rowSecret,
		"draft":          a.draft,
		"draftId":        a.draftID,
		"used":           used,
		"answerAsPrompt": asPrompt,
		"secretAsInput":  secretIn,
		"dupId":          dup,
		"wrongAnswer":    wrong,
	}, nil
}

func (a *askRRAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
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

// until polls ok for up to d; the steps that wait on something the spec
// says happens (an answer routed, an ask cancelled) do not fail when it
// does not: the state check after the step names what differs.
func until(d time.Duration, ok func() bool) bool {
	deadline := time.Now().Add(d)
	for !ok() {
		if time.Now().After(deadline) {
			return false
		}
		time.Sleep(20 * time.Millisecond)
	}
	return true
}

// closed says whether the k-th ask is no longer open in history.
func (a *askRRAdapter) closed(k int) func() bool {
	return func() bool {
		v, err := a.view()
		return err == nil && v.open != k
	}
}

func (a *askRRAdapter) put(tool string, args map[string]any, asked int) error {
	a.seq++
	a.specIDs = append(a.specIDs, fmt.Sprintf("g%d-ask-%d", a.gen, a.seq))
	a.genOf = append(a.genOf, a.gen)
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: tool, Args: args})
	a.held = ""
	if !until(actionTimeout, func() bool {
		v, err := a.view()
		return err == nil && len(v.asks) == asked+1 && v.open == asked && v.armed && v.rowK == asked
	}) {
		return fmt.Errorf("%s: the ask never opened, armed and on screen", tool)
	}
	return nil
}

func (a *askRRAdapter) askEnabled(v rrView) bool {
	return v.row.Live && a.uiUp && v.open < 0 && a.line == "" && len(v.asks) < 2
}

func (a *askRRAdapter) AskPlain() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.askEnabled(v)) {
		return nil
	}
	return a.put("ask", map[string]any{"question": fmt.Sprintf("Which colour for q%d?", len(v.asks)+1), "options": []string{"red", "blue"}}, len(v.asks))
}

func (a *askRRAdapter) AskSecret() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.askEnabled(v)) {
		return nil
	}
	k := len(v.asks)
	return a.put("secret", map[string]any{"name": fmt.Sprintf("TOKEN_Q%d", k+1), "question": "the API token", "project": askProject}, k)
}

func (a *askRRAdapter) ReloadUi() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && a.uiUp) {
		return nil
	}
	a.uiUp = false
	if a.reloadUiNoop {
		return nil
	}
	return a.writeConfig(a.timeout, false, true)
}

// RemountUi enables the ui row again. A line held in the gap is routed
// by the remounted router: it answers the ask it was sent for, if that
// is still open, and the model's next request follows.
func (a *askRRAdapter) RemountUi() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && !a.uiUp) {
		return nil
	}
	a.uiUp = true
	if err := a.writeConfig(a.timeout, true, true); err != nil {
		return err
	}
	if a.line == "" {
		return nil
	}
	a.line, a.lineSrc, a.lineProd = "", "", ""
	if v.open >= 0 && until(5*time.Second, a.closed(v.open)) {
		return a.takeNext()
	}
	return nil
}

// ReloadAsk changes the ask row's config: the kernel remounts it and the
// ui row with it. Its open ask ends (the call returns) and the model's
// next request follows. Then the ui row is disabled, to hold open the
// gap the spec leaves after this step.
func (a *askRRAdapter) ReloadAsk() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && a.uiUp && a.reloads < 1) {
		return nil
	}
	a.reloads++
	a.gen++
	a.seq = 0
	a.uiUp = false
	if err := a.writeConfig(a.timeout+1, true, true); err != nil {
		return err
	}
	if v.open >= 0 && until(5*time.Second, a.closed(v.open)) {
		if err := a.takeNext(); err != nil {
			return err
		}
	}
	return a.writeConfig(a.timeout, false, true)
}

// deliver POSTs an answer written for question src to the product ask
// prod: serve writes the line and disarms. With the ui row mounted the
// router answers the open ask; in the gap the line is held.
func (a *askRRAdapter) deliver(v rrView, src, prod string, secret bool) error {
	a.n++
	text := fmt.Sprintf("ans-%s-%d", src, a.n)
	if secret {
		text = fmt.Sprintf("S3CR3T-%s-%d", src, a.n)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": text, "ask": prod})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("answer %s: %d %s", prod, code, msg)
	}
	if !a.uiUp {
		a.line, a.lineSrc, a.lineProd = "ans", src, prod
		if secret {
			a.line = "sec"
		}
		return nil
	}
	if v.open < 0 {
		return nil
	}
	if !until(actionTimeout, a.closed(v.open)) {
		return fmt.Errorf("answer %s: the ask stayed open", prod)
	}
	return a.takeNext()
}

func (a *askRRAdapter) Answer() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.rowK >= 0 && v.armed && v.rowK == len(v.asks)-1) {
		return nil
	}
	return a.deliver(v, qOf(v.rowK), v.row.Ask.ID, v.row.Ask.Secret)
}

// TypeDraft: typing into the composer while a plain question is on
// screen starts an answer draft keyed to it (draftAsk, the product id).
func (a *askRRAdapter) TypeDraft() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.draft == "" && v.rowK >= 0 && !v.row.Ask.Secret) {
		return nil
	}
	a.draft, a.draftID, a.draftProd = qOf(v.rowK), a.specID(v.rowK), v.row.Ask.ID
	return nil
}

// UseForQuestion is offered when askChanged: the page compares draftAsk
// with the row's ask id, both the product's.
func (a *askRRAdapter) UseForQuestion() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.draft != "" && v.rowK >= 0 && !v.row.Ask.Secret && a.draftProd != v.row.Ask.ID) {
		return nil
	}
	a.draft, a.draftID, a.draftProd = qOf(v.rowK), a.specID(v.rowK), v.row.Ask.ID
	return nil
}

// SendDraft: Send is enabled when askChanged is false; it POSTs the
// draft with its draftAsk.
func (a *askRRAdapter) SendDraft() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.draft != "" && v.row.Live && v.rowK >= 0 && a.draftProd == v.row.Ask.ID && v.armed) {
		return nil
	}
	src, prod := a.draft, a.draftProd
	a.draft, a.draftID, a.draftProd = "", "", ""
	return a.deliver(v, src, prod, false)
}

// Timeout times the open ask out through BOUGH_TEST_ASK_EXPIRE_DIR.
func (a *askRRAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.open >= 0) {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.expire, v.asks[v.open].prod), nil, 0o644); err != nil {
		return err
	}
	if !until(actionTimeout, a.closed(v.open)) {
		return errors.New("timeout: the ask stayed open")
	}
	return a.takeNext()
}

// Exit kills the child. Its next process reads the config fresh, so a
// disabled ui row is enabled again first (no reload: nothing is alive).
func (a *askRRAdapter) Exit() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live) {
		return nil
	}
	a.held = ""
	a.line, a.lineSrc, a.lineProd = "", "", ""
	if err := a.kill(); err != nil {
		return err
	}
	a.uiUp = true
	return a.writeConfig(a.timeout, true, false)
}

// Respawn is a prompt to the dead session: serve starts a fresh child
// on the same history, and its turn's first request is held.
func (a *askRRAdapter) Respawn() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!v.row.Live && a.respawns < 1) {
		return nil
	}
	a.respawns++
	a.gen++
	a.seq = 0
	ctx, cancel := actionCtx()
	defer cancel()
	if code, msg, err := a.post(ctx, "prompt", map[string]string{"text": "respawn " + a.next}); err != nil {
		return err
	} else if code != http.StatusOK {
		return fmt.Errorf("respawn prompt: %d %s", code, msg)
	}
	if err := a.takeNext(); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Live && r.Status == serve.StatusRunning })
	return err
}

func rrCounted(name string, f func(*askRRAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*askRRAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var askRRActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AskPlain":       rrCounted("AskPlain", (*askRRAdapter).AskPlain),
	"AskSecret":      rrCounted("AskSecret", (*askRRAdapter).AskSecret),
	"ReloadUi":       rrCounted("ReloadUi", (*askRRAdapter).ReloadUi),
	"RemountUi":      rrCounted("RemountUi", (*askRRAdapter).RemountUi),
	"ReloadAsk":      rrCounted("ReloadAsk", (*askRRAdapter).ReloadAsk),
	"Answer":         rrCounted("Answer", (*askRRAdapter).Answer),
	"TypeDraft":      rrCounted("TypeDraft", (*askRRAdapter).TypeDraft),
	"UseForQuestion": rrCounted("UseForQuestion", (*askRRAdapter).UseForQuestion),
	"SendDraft":      rrCounted("SendDraft", (*askRRAdapter).SendDraft),
	"Timeout":        rrCounted("Timeout", (*askRRAdapter).Timeout),
	"Exit":           rrCounted("Exit", (*askRRAdapter).Exit),
	"Respawn":        rrCounted("Respawn", (*askRRAdapter).Respawn),
}, "": {
	// deadlock_detection is off, so fizz links a terminal state (the
	// child dead after its one respawn) to itself as "end", and the
	// runner offers it everywhere. Taken, it matches no link and the
	// server stops checking there, so it declines like a disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*askRRAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func askRROptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// askRRHistory reads an abstract trace off a transcript. The page's
// steps and a remount with nothing open leave nothing in history, so a
// trace is the asks, their ends and the respawn: an answer is Answer; a
// call that ends unanswered is Timeout when it timed out and ReloadAsk,
// RemountUi when its Asker was disposed; an input after the first is the
// respawn's prompt (Exit, Respawn). The check is on asked and open.
func askRRHistory(entries []history.Entry) []tracecheck.Step {
	st := func(asked int, open string) map[string]any {
		return map[string]any{"Session#0.asked": asked, "Session#0.open": open}
	}
	asked, open, openID, answered, first := 0, "", "", false, true
	steps := []tracecheck.Step{{Action: "Init", State: st(0, "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if first {
				first = false
				continue
			}
			add("Exit", st(asked, ""))
			add("Respawn", st(asked, ""))
			open = ""
		case "ask":
			asked++
			open, openID, answered = fmt.Sprintf("q%d", asked), str(e.Data["id"]), false
			if s, _ := e.Data["secret"].(bool); s {
				add("AskSecret", st(asked, open))
			} else {
				add("AskPlain", st(asked, open))
			}
		case "ask/answer":
			if open != "" && str(e.Data["id"]) == openID {
				add("Answer", st(asked, ""))
				answered = true
			}
		case "call":
			if t := str(e.Data["tool"]); (t == "ask" || t == "secret") && open != "" {
				open = ""
				if answered {
					continue
				}
				if b, _ := json.Marshal(e.Data); strings.Contains(string(b), "no answer after") {
					add("Timeout", st(asked, ""))
				} else {
					add("ReloadAsk", st(asked, ""))
					add("RemountUi", st(asked, ""))
				}
			}
		}
	}
	return steps
}

func init() { historyProjections["ask_across_reload_respawn"] = askRRHistory }

func TestAskAcrossReloadRespawn(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAskRRAdapter(t)
	if err := runMBT(t, "ask_across_reload_respawn", a, askRRActions, askRROptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g := loadAskRRGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), askRRHistory)
	}
}

func askRRPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("ask_across_reload_respawn", cover)
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

// walkAskRRPath drives one path and compares the whole state after every
// step; the first difference ends the path.
func walkAskRRPath(a *askRRAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		gb, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gb, &g)
		var diffs []string
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(g[field]) != fmt.Sprint(v) {
				diffs = append(diffs, fmt.Sprintf("%s = %v, want %v", field, g[field], v))
			}
		}
		if len(diffs) > 0 {
			return fmt.Errorf("step %d (%s): %s\n  got  %s", i, path[i].Action, strings.Join(diffs, "; "), gb)
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
		name := strings.TrimPrefix(path[i].Action, "Session#0.")
		if name == "end" {
			// A terminal state's self-link: nothing happens, and the
			// state must still be the same.
			if err := check(i, path[i].State); err != nil {
				return err
			}
			continue
		}
		fn, ok := askRRActions["Session"][name]
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

func pathActions(p []tracecheck.Step) []string {
	var acts []string
	for _, st := range p[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Session#0."))
	}
	return acts
}

// TestAskAcrossReloadRespawnPaths walks every generated path against a
// real serve, four serves sharing them, then replays each transcript.
func TestAskAcrossReloadRespawnPaths(t *testing.T) {
	t.Parallel()
	paths := askRRPaths(t, envCover())
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newAskRRAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkAskRRPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, pathActions(paths[i]), err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			g := loadAskRRGraph(t)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), askRRHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrongly wired adapter fails it: with
// ReloadUi a no-op, an answer "in the gap" is routed at once, where the
// spec holds it. That shows on the ReloadUi, Answer transition.
func TestAskAcrossReloadRespawnPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newAskRRAdapter(t)
	a.reloadUiNoop = true
	for _, p := range askRRPaths(t, tracecheck.CoverTransitions) {
		held := false
		for i := 1; i < len(p); i++ {
			if p[i].Action == "Session#0.Answer" && p[i-1].Action == "Session#0.ReloadUi" {
				held = true
			}
		}
		if !held {
			continue
		}
		if err := walkAskRRPath(a, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with ReloadUi a no-op; the walk is not checking state")
}

func loadAskRRGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("ask_across_reload_respawn")), "..", "testdata", "ask_across_reload_respawn"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
