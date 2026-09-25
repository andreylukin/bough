//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
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

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/secret_store_outcomes.fizz against a real serve: two local
// sessions (children A and B) each call tools.secret for one name in the
// same project while the person renames the project, edits project.yml
// in the web editor or deletes the project.
//
// Each child's model is llm-control: its first request makes the native
// secret call, and the one after the call finds the queue empty and ends
// the turn. The child is started only when the walk asks, so a walk that
// never asks spawns nothing. Between the answer, the keychain write and
// SetSecret the child waits on BOUGH_TEST_SECRET_STEP_DIR, which is how a
// walk puts the person's steps (or the other child's) in between; the
// keychain is the file keychain (BOUGH_TEST_KEYCHAIN_DIR), and a store
// fails because a directory sits where the value would be written. The
// serve runs with BOUGH_CONTAINER=none: no step here touches a container.

// ssoText is project.yml as every walk starts it: a shipped comment, the
// person's own comment and an env block after them, so a writer that
// re-marshals the file shows as lost comments.
const ssoText = `# bough project definition. Lives outside every repo; never committed.
name: Alpha
repos: []
# the person's own note: keep this
env:
  LOG_LEVEL: debug
`

var ssoNames = [2]string{"A_TOKEN", "B_TOKEN"}

type ssoChild struct {
	id, cwd string // "" until the walk asks
}

// ssoAdapter plays the page (the editor's loaded text and whether the
// rename was answered ok are its own) and reads the rest off the serve,
// the transcripts, project.yml and the keychain dir.
type ssoAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	keychain string
	steps    string
	gate     gate

	walk int
	slug string
	kids [2]ssoChild
	n    int // turn names, unique across walks
	ids  []string
	did  map[string]int

	renamed  bool
	lastName string
	comments bool
	gone     [2]bool // the ref was set when the project was deleted
	editor   bool
	edText   string

	// blindSave is the deliberate bug the wrong-adapter test injects: the
	// editor's stale save carries no base, as a page that forgot it would.
	blindSave bool
	// wrongName is the random run's deliberate bug: the rename asks the
	// server for a name the spec never has.
	wrongName bool
}

func newSSOAdapter(t *testing.T) *ssoAdapter {
	keychain, steps := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Env: []string{
			"BOUGH_CONTAINER=none",
			"BOUGH_TEST_KEYCHAIN_DIR=" + keychain,
			"BOUGH_TEST_SECRET_STEP_DIR=" + steps,
		},
	})
	return &ssoAdapter{t: t, s: s, dir: control.Dir(s.Home), keychain: keychain, steps: steps, did: map[string]int{}}
}

func (a *ssoAdapter) projectDir() string { return filepath.Join(projectdef.Root(a.s.Home), a.slug) }

// Init starts each walk on a project of its own, written the way an
// agent would (a project is its directory), so no walk sees another's
// keychain entries or refs.
func (a *ssoAdapter) Init() error {
	a.walk++
	a.slug = fmt.Sprintf("sso-%d", a.walk)
	if err := os.MkdirAll(a.projectDir(), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.projectDir(), projectdef.FileYAML), []byte(ssoText), 0o644); err != nil {
		return err
	}
	a.kids = [2]ssoChild{}
	a.renamed, a.lastName, a.comments = false, "Alpha", true
	a.gone = [2]bool{}
	a.editor, a.edText = false, ""
	a.gate.reset()
	return nil
}

// Cleanup kills the walk's children, which sit idle (or parked on a
// step) once their one call is over.
func (a *ssoAdapter) Cleanup() error {
	var errs []error
	for _, k := range a.kids {
		if k.id != "" {
			errs = append(errs, ssoKillCwd(a.s, k.id, k.cwd))
		}
	}
	return errors.Join(errs...)
}

// ssoKillCwd SIGKILLs the child whose cwd is cwd (a created session's
// command line does not carry its id) and waits for serve to see it gone.
func ssoKillCwd(s *servetest.Server, id, cwd string) error {
	out, _ := exec.Command("lsof", "-a", "-d", "cwd", "-c", "bough", "-Fpn").Output()
	pid := 0
	for _, l := range strings.Split(string(out), "\n") {
		switch {
		case strings.HasPrefix(l, "p"):
			pid, _ = strconv.Atoi(l[1:])
		case strings.HasPrefix(l, "n") && l[1:] == cwd && pid > 0:
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	_, err := waitRow(s, id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *ssoAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// ssoKid is one child's part of the state, read off its transcript and
// the keychain.
type ssoKid struct {
	phase, hist string
	kc          bool
	askID       string
}

func (a *ssoAdapter) kcFile(i int) string {
	return filepath.Join(a.keychain, "bough%"+a.slug+"%"+ssoNames[i])
}

// holds is the keychain holding child i's value: a regular file (the
// directory that makes a store fail is not a value).
func (a *ssoAdapter) holds(i int) bool {
	st, err := os.Stat(a.kcFile(i))
	return err == nil && st.Mode().IsRegular()
}

// secretEnd is a transcript's recorded end of the secret call: what the
// tool returned, or its error.
func secretEnd(e history.Entry) (output, errText string, ok bool) {
	if e.Kind != "call" || str(e.Data["tool"]) != "secret" || str(e.Data["phase"]) == "start" {
		return "", "", false
	}
	return str(e.Data["output"]), str(e.Data["error"]), true
}

// histOf is what a transcript says about the secret. An answer line
// that claims "[secret stored]" says stored whatever followed it.
func histOf(answer string, answered bool, output, errText string, ended bool) string {
	h := ""
	switch {
	case ended && strings.Contains(errText, "declined"):
		h = "declined"
	case ended && errText != "":
		h = "failed"
	case ended && strings.HasPrefix(output, "stored "):
		h = "stored"
	case ended:
		h = "ended:" + output // a mismatch that names itself
	case answered:
		h = "received"
	}
	if answered && answer == "[secret stored]" {
		h = "stored"
	}
	return h
}

func (a *ssoAdapter) kid(i int) (ssoKid, error) {
	k := ssoKid{phase: "idle", kc: a.holds(i)}
	c := a.kids[i]
	if c.id == "" {
		return k, nil
	}
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", c.id+".jsonl"))
	if err != nil {
		return k, err
	}
	var asked, answered, ended bool
	var answer, output, errText string
	for _, e := range entries {
		if e.Kind == "ask" && strings.Contains(str(e.Data["question"]), ssoNames[i]) {
			asked, k.askID = true, str(e.Data["id"])
		}
		if e.Kind == "ask/answer" && asked && str(e.Data["id"]) == k.askID {
			answered, answer = true, str(e.Data["text"])
		}
		if o, er, ok := secretEnd(e); ok {
			ended, output, errText = true, o, er
		}
	}
	switch {
	case ended:
		k.phase = "over"
	case answered && k.kc:
		k.phase = "kc"
	case answered:
		k.phase = "answered"
	case asked:
		k.phase = "asked"
	}
	k.hist = histOf(answer, answered, output, errText, ended)
	return k, nil
}

// ssoView is the spec's state as the server, the files and the page
// have it.
type ssoView struct {
	disk     string
	name     string
	comments bool
	ref      [2]string
	kid      [2]ssoKid
}

func (a *ssoAdapter) view() (ssoView, error) {
	v := ssoView{disk: "none", name: a.lastName, comments: a.comments}
	b, err := os.ReadFile(filepath.Join(a.projectDir(), projectdef.FileYAML))
	switch {
	case err == nil:
		v.disk = "ok"
		d, perr := projectdef.Parse(b)
		if perr != nil {
			v.name = "unparsable: " + perr.Error()
		} else {
			v.name = d.Name
		}
		text := string(b)
		v.comments = strings.Contains(text, "# bough project definition.") && strings.Contains(text, "# the person's own note: keep this")
		for i, n := range ssoNames {
			if d.Secrets[n] != "" {
				v.ref[i] = "set"
			}
		}
		a.lastName, a.comments = v.name, v.comments
	case errors.Is(err, os.ErrNotExist):
		for i := range v.ref {
			if a.gone[i] {
				v.ref[i] = "gone"
			}
		}
	default:
		return v, err
	}
	for i := range v.kid {
		if v.kid[i], err = a.kid(i); err != nil {
			return v, err
		}
	}
	return v, nil
}

// edited is the editor's loaded text as the spec has it: its name line
// and which refs it carries ("" while closed).
func (a *ssoAdapter) edited() (name string, refs [2]string) {
	if !a.editor {
		return "", refs
	}
	d, err := projectdef.Parse([]byte(a.edText))
	if err != nil {
		return "unparsable: " + err.Error(), refs
	}
	for i, n := range ssoNames {
		if d.Secrets[n] != "" {
			refs[i] = "set"
		}
	}
	return d.Name, refs
}

func (a *ssoAdapter) state(v ssoView) map[string]any {
	edName, edRefs := a.edited()
	editor := "closed"
	if a.editor {
		editor = "open"
	}
	st := map[string]any{
		"disk":     v.disk,
		"name":     v.name,
		"renamed":  a.renamed,
		"comments": v.comments,
		"editor":   editor,
		"edName":   edName,
		"edA":      edRefs[0],
		"edB":      edRefs[1],
	}
	for i, x := range []string{"a", "b"} {
		st[x+"Phase"] = v.kid[i].phase
		st[x+"Hist"] = v.kid[i].hist
		st[x+"Kc"] = v.kid[i].kc
		st[x+"Ref"] = v.ref[i]
	}
	return st
}

func (a *ssoAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return a.state(v), nil
}

// call is one API request; a non-2xx answer is an *servetest.APIError.
func (a *ssoAdapter) call(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		var e struct {
			Error string `json:"error"`
		}
		json.Unmarshal(raw, &e)
		return &servetest.APIError{Status: resp.StatusCode, Msg: e.Error}
	}
	if out != nil {
		return json.Unmarshal(raw, out)
	}
	return nil
}

// act gates an action on the spec's require, read off a fresh view.
func (a *ssoAdapter) act(name string, require func(v ssoView) bool, do func(v ssoView) error) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(v)) {
		return nil
	}
	if err := do(v); err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	return nil
}

// waitKid polls child i's view until ok holds.
func (a *ssoAdapter) waitKid(i int, what string, ok func(ssoKid) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		k, err := a.kid(i)
		if err != nil {
			return err
		}
		if ok(k) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: child %s is %+v after %s", what, ssoNames[i], k, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// settle waits for child i's call to end and then for its turn to close:
// the request after the call finds the queue empty and finishes, and it
// must not be left to take a turn queued for the next child.
func (a *ssoAdapter) settle(i int) error {
	if err := a.waitKid(i, "the secret call to end", func(k ssoKid) bool { return k.phase == "over" }); err != nil {
		return err
	}
	_, err := waitRow(a.s, a.kids[i].id, "the turn to close", func(r serve.Row) bool { return r.Status == serve.StatusDone })
	return err
}

func (a *ssoAdapter) ask(i int) error {
	return a.act("AskSecret"+"AB"[i:i+1], func(v ssoView) bool { return v.disk == "ok" && v.kid[i].phase == "idle" }, func(ssoView) error {
		a.n++
		turn := fmt.Sprintf("s%06d", a.n)
		control.Queue(a.t, a.dir, turn, control.Turn{Mode: "call", Tool: "secret", Args: map[string]any{
			"name": ssoNames[i], "question": "the API token", "project": a.slug,
		}})
		cwd := a.s.Dir(a.t, fmt.Sprintf("%s-%d", a.slug, i))
		ctx, cancel := actionCtx()
		defer cancel()
		row, err := a.s.CreateSession(ctx, cwd, "store "+ssoNames[i])
		if err != nil {
			return err
		}
		a.kids[i] = ssoChild{id: row.ID, cwd: cwd}
		a.ids = append(a.ids, row.ID)
		if err := waitTaken(a.dir, turn); err != nil {
			return err
		}
		_, err = waitRow(a.s, row.ID, "the secret question", func(r serve.Row) bool { return r.Ask != nil && r.Ask.Secret })
		return err
	})
}

func (a *ssoAdapter) answer(i int, text string) error {
	k, err := a.kid(i)
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	b, _ := json.Marshal(map[string]string{"text": text, "ask": k.askID})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+a.kids[i].id+"/answer", bytes.NewReader(b))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("answer: %d", resp.StatusCode)
	}
	return nil
}

func (a *ssoAdapter) answerSecret(i int) error {
	return a.act("AnswerSecret"+"AB"[i:i+1], func(v ssoView) bool { return v.kid[i].phase == "asked" }, func(ssoView) error {
		a.n++
		if err := a.answer(i, fmt.Sprintf("S3CR3T-%d ", a.n)); err != nil {
			return err
		}
		// The child now holds the value and waits for the store step.
		return a.waitKid(i, "the answer line", func(k ssoKid) bool { return k.phase != "asked" })
	})
}

func (a *ssoAdapter) decline(i int) error {
	return a.act("Decline"+"AB"[i:i+1], func(v ssoView) bool { return v.kid[i].phase == "asked" }, func(ssoView) error {
		if err := a.answer(i, "(declined)"); err != nil {
			return err
		}
		return a.settle(i)
	})
}

func (a *ssoAdapter) stepFile(i int, what string) error {
	return os.WriteFile(filepath.Join(a.steps, a.slug+"."+ssoNames[i]+"."+what), nil, 0o644)
}

func (a *ssoAdapter) keychainOk(i int) error {
	return a.act("KeychainStoreOk"+"AB"[i:i+1], func(v ssoView) bool { return v.kid[i].phase == "answered" }, func(ssoView) error {
		if err := a.stepFile(i, "store"); err != nil {
			return err
		}
		return a.waitKid(i, "the keychain write", func(k ssoKid) bool { return k.kc || k.phase == "over" })
	})
}

// keychainFail makes the store fail: a directory where the file
// keychain writes the value. It goes again once the call has failed.
func (a *ssoAdapter) keychainFail(i int) error {
	return a.act("KeychainStoreFail"+"AB"[i:i+1], func(v ssoView) bool { return v.kid[i].phase == "answered" }, func(ssoView) error {
		if err := os.Mkdir(a.kcFile(i), 0o700); err != nil {
			return err
		}
		defer os.Remove(a.kcFile(i))
		if err := a.stepFile(i, "store"); err != nil {
			return err
		}
		return a.settle(i)
	})
}

func (a *ssoAdapter) setSecret(i int) error {
	return a.act("SetSecret"+"AB"[i:i+1], func(v ssoView) bool { return v.kid[i].phase == "kc" }, func(ssoView) error {
		if err := a.stepFile(i, "set"); err != nil {
			return err
		}
		return a.settle(i)
	})
}

func (a *ssoAdapter) AskSecretA() error         { return a.ask(0) }
func (a *ssoAdapter) AskSecretB() error         { return a.ask(1) }
func (a *ssoAdapter) AnswerSecretA() error      { return a.answerSecret(0) }
func (a *ssoAdapter) AnswerSecretB() error      { return a.answerSecret(1) }
func (a *ssoAdapter) DeclineA() error           { return a.decline(0) }
func (a *ssoAdapter) DeclineB() error           { return a.decline(1) }
func (a *ssoAdapter) KeychainStoreOkA() error   { return a.keychainOk(0) }
func (a *ssoAdapter) KeychainStoreOkB() error   { return a.keychainOk(1) }
func (a *ssoAdapter) KeychainStoreFailA() error { return a.keychainFail(0) }
func (a *ssoAdapter) KeychainStoreFailB() error { return a.keychainFail(1) }
func (a *ssoAdapter) SetSecretA() error         { return a.setSecret(0) }
func (a *ssoAdapter) SetSecretB() error         { return a.setSecret(1) }

func (a *ssoAdapter) RenameProject() error {
	return a.act("RenameProject", func(v ssoView) bool { return v.disk == "ok" && v.name == "Alpha" }, func(ssoView) error {
		name := "Beta"
		if a.wrongName {
			name = "Gamma"
		}
		if err := a.call(http.MethodPost, "/api/projects/"+a.slug+"/rename", map[string]string{"name": name}, nil); err != nil {
			return err
		}
		a.renamed = true
		return nil
	})
}

// DeleteProject is the page's delete after its typed confirm. The page
// the editor was on goes with it.
func (a *ssoAdapter) DeleteProject() error {
	return a.act("DeleteProject", func(v ssoView) bool { return v.disk == "ok" }, func(v ssoView) error {
		if err := a.call(http.MethodDelete, "/api/projects/"+a.slug, nil, nil); err != nil {
			return err
		}
		for i := range a.gone {
			a.gone[i] = v.ref[i] == "set"
		}
		a.editor, a.edText = false, ""
		return nil
	})
}

// load is the editor reading project.yml the way the orb page does.
func (a *ssoAdapter) load() error {
	var d serve.OrbDetail
	if err := a.call(http.MethodGet, "/api/projects/"+a.slug+"/orb", nil, &d); err != nil {
		return err
	}
	a.editor, a.edText = true, d.Files[projectdef.FileYAML]
	return nil
}

func (a *ssoAdapter) EditOpen() error {
	return a.act("EditOpen", func(v ssoView) bool { return v.disk == "ok" && !a.editor }, func(ssoView) error { return a.load() })
}

// current is whether the editor's text is still what the file says, as
// far as the spec's fields go.
func (a *ssoAdapter) current(v ssoView) bool {
	name, refs := a.edited()
	return a.editor && name == v.name && refs == v.ref
}

// save PUTs the editor's text with the person's own line added, based on
// the text the editor loaded.
func (a *ssoAdapter) save(blind bool) error {
	body := map[string]any{"text": a.edText + "# saved from the editor\n"}
	if !blind {
		body["base"] = a.edText
	}
	return a.call(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/"+projectdef.FileYAML, body, nil)
}

func (a *ssoAdapter) EditSave() error {
	return a.act("EditSave", func(v ssoView) bool { return a.current(v) }, func(ssoView) error {
		if err := a.save(false); err != nil {
			return err
		}
		a.editor, a.edText = false, ""
		return nil
	})
}

// EditSaveStale saves after another writer changed the file: refused
// with 409, and the editor reloads. A save that is taken closes the
// editor as a successful one does, and the walk sees what it undid.
func (a *ssoAdapter) EditSaveStale() error {
	return a.act("EditSaveStale", func(v ssoView) bool { return a.editor && !a.current(v) }, func(ssoView) error {
		err := a.save(a.blindSave)
		var apiErr *servetest.APIError
		switch {
		case err == nil:
			a.editor, a.edText = false, ""
			return nil
		case errors.As(err, &apiErr) && apiErr.Status == http.StatusConflict:
			return a.load()
		default:
			return err
		}
	})
}

func (a *ssoAdapter) EditCancel() error {
	return a.act("EditCancel", func(ssoView) bool { return a.editor }, func(ssoView) error {
		a.editor, a.edText = false, ""
		return nil
	})
}

// end is fizz's self-link on a state with nothing enabled: the project
// deleted, the editor closed and neither child mid-call. The runner
// offers it in every state as a role-less action; the library
// dereferences a missing one (a nil-pointer panic that took the test
// binary down), and a pick anywhere else is a disabled one.
func (a *ssoAdapter) end() error {
	quiet := func(k ssoKid) bool { return k.phase == "idle" || k.phase == "over" }
	return a.act("end", func(v ssoView) bool {
		return v.disk != "ok" && !a.editor && quiet(v.kid[0]) && quiet(v.kid[1])
	}, func(ssoView) error { return nil })
}

func ssoAction(name string, f func(*ssoAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*ssoAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var ssoActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"AskSecretA":         ssoAction("AskSecretA", (*ssoAdapter).AskSecretA),
	"AskSecretB":         ssoAction("AskSecretB", (*ssoAdapter).AskSecretB),
	"AnswerSecretA":      ssoAction("AnswerSecretA", (*ssoAdapter).AnswerSecretA),
	"AnswerSecretB":      ssoAction("AnswerSecretB", (*ssoAdapter).AnswerSecretB),
	"DeclineA":           ssoAction("DeclineA", (*ssoAdapter).DeclineA),
	"DeclineB":           ssoAction("DeclineB", (*ssoAdapter).DeclineB),
	"KeychainStoreOkA":   ssoAction("KeychainStoreOkA", (*ssoAdapter).KeychainStoreOkA),
	"KeychainStoreOkB":   ssoAction("KeychainStoreOkB", (*ssoAdapter).KeychainStoreOkB),
	"KeychainStoreFailA": ssoAction("KeychainStoreFailA", (*ssoAdapter).KeychainStoreFailA),
	"KeychainStoreFailB": ssoAction("KeychainStoreFailB", (*ssoAdapter).KeychainStoreFailB),
	"SetSecretA":         ssoAction("SetSecretA", (*ssoAdapter).SetSecretA),
	"SetSecretB":         ssoAction("SetSecretB", (*ssoAdapter).SetSecretB),
	"RenameProject":      ssoAction("RenameProject", (*ssoAdapter).RenameProject),
	"DeleteProject":      ssoAction("DeleteProject", (*ssoAdapter).DeleteProject),
	"EditOpen":           ssoAction("EditOpen", (*ssoAdapter).EditOpen),
	"EditSave":           ssoAction("EditSave", (*ssoAdapter).EditSave),
	"EditSaveStale":      ssoAction("EditSaveStale", (*ssoAdapter).EditSaveStale),
	"EditCancel":         ssoAction("EditCancel", (*ssoAdapter).EditCancel),
}, "": {
	"end": ssoAction("end", (*ssoAdapter).end),
}}

// secretStoreOutcomesHistory reads one child's transcript: which child
// it is comes from the name in its question. The person's steps leave
// nothing there; a SetSecret that failed on a missing project is the
// DeleteProject that came before it. The check is on the child's phase
// and what its transcript says.
func secretStoreOutcomesHistory(entries []history.Entry) []tracecheck.Step {
	x, X := "a", "A"
	for _, e := range entries {
		if e.Kind == "ask" && strings.Contains(str(e.Data["question"]), ssoNames[1]) {
			x, X = "b", "B"
		}
	}
	st := func(phase, hist string) map[string]any {
		return map[string]any{"Project#0." + x + "Phase": phase, "Project#0." + x + "Hist": hist}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("idle", "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Project#0." + action + X, State: state})
	}
	answered, answer := false, ""
	for _, e := range entries {
		switch e.Kind {
		case "ask":
			if s, _ := e.Data["secret"].(bool); s {
				add("AskSecret", st("asked", ""))
			}
		case "ask/answer":
			answer = str(e.Data["text"])
			if answer != "(declined)" {
				answered = true
				add("AnswerSecret", st("answered", histOf(answer, true, "", "", false)))
			}
		}
		output, errText, ok := secretEnd(e)
		if !ok {
			continue
		}
		hist := histOf(answer, answered, output, errText, true)
		switch {
		case strings.Contains(errText, "declined"):
			add("Decline", st("over", hist))
		case strings.Contains(errText, "store failed"):
			add("KeychainStoreFail", st("over", hist))
		case errText != "":
			add("KeychainStoreOk", st("kc", "received"))
			steps = append(steps, tracecheck.Step{Action: "Project#0.DeleteProject"})
			add("SetSecret", st("over", hist))
		default:
			add("KeychainStoreOk", st("kc", "received"))
			add("SetSecret", st("over", hist))
		}
	}
	return steps
}

func init() { historyProjections["secret_store_outcomes"] = secretStoreOutcomesHistory }

func ssoOptions() map[string]any {
	return map[string]any{"max-seq-runs": 1000, "max-actions": 10, "max-parallel-runs": 0}
}

func loadSSOGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("secret_store_outcomes")), "..", "testdata", "secret_store_outcomes"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// TestSecretStoreOutcomes is the runner's random walks (the exhaustive
// run only; see runMBT).
func TestSecretStoreOutcomes(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSSOAdapter(t)
	if err := runMBT(t, "secret_store_outcomes", a, ssoActions, ssoOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g := loadSSOGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), secretStoreOutcomesHistory)
	}
}

// The random run proves nothing unless a wrongly wired adapter fails it:
// a rename to a name the spec never has must be caught. RenameProject is
// enabled at Init, so a few hundred walks pick it many times.
func TestSecretStoreOutcomesCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSSOAdapter(t)
	a.wrongName = true
	if err := runMBT(t, "secret_store_outcomes", a, ssoActions, ssoOptions()); err == nil {
		t.Fatal("a run whose rename writes Gamma passed; the runner is not checking state")
	}
}

func ssoPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("secret_store_outcomes", cover)
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

// walkSSOPath drives one generated path and compares the whole state
// after every step; the first difference ends the path.
func walkSSOPath(a *ssoAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		var diffs []string
		for k, want := range path[i].State {
			field, ok := strings.CutPrefix(k, "Project#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(got[field]) != fmt.Sprint(want) {
				diffs = append(diffs, fmt.Sprintf("%s = %v, want %v", field, got[field], want))
			}
		}
		if len(diffs) > 0 {
			gb, _ := json.Marshal(got)
			return fmt.Errorf("step %d (%s): %s\n  got %s", i, path[i].Action, strings.Join(diffs, "; "), gb)
		}
		return nil
	}
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0); err != nil {
		return err
	}
	for i := 1; i < len(path); i++ {
		name := strings.TrimPrefix(path[i].Action, "Project#0.")
		if name == "end" {
			// fizz's self-link on a state with no action out of it: the
			// state is read again, and it must still be the same.
			if err := check(i); err != nil {
				return err
			}
			continue
		}
		fn, ok := ssoActions["Project"][name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		if _, err := fn(a, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i); err != nil {
			return err
		}
	}
	return nil
}

func ssoActs(path []tracecheck.Step) []string {
	var acts []string
	for _, st := range path[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Project#0."))
	}
	return acts
}

// TestSecretStoreOutcomesPaths walks every generated path (every settled
// state; every link under MODEL_COVER=transitions) against real serves,
// then replays each child's transcript on the graph.
func TestSecretStoreOutcomesPaths(t *testing.T) {
	t.Parallel()
	b, err := pathsJSON("secret_store_outcomes")
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
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newSSOAdapter(t)
			for i := s; i < len(f.Paths); i += shards {
				path := f.Paths[i].Trace
				if err := walkSSOPath(a, path); err != nil {
					t.Errorf("path %d %v: %v", i, ssoActs(path), err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			g := loadSSOGraph(t)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), secretStoreOutcomesHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrongly wired adapter fails it: an
// editor save that carries no base must be caught undoing a rename or a
// ref. It is one transition (EditSaveStale), so every link is walked,
// and the first failing path is enough.
func TestSecretStoreOutcomesPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSSOAdapter(t)
	a.blindSave = true
	for _, p := range ssoPaths(t, tracecheck.CoverTransitions) {
		if !containsAction(p, "Project#0.EditSaveStale") {
			continue
		}
		if err := walkSSOPath(a, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with the editor saving blind; the walk is not checking state")
}

func containsAction(path []tracecheck.Step, action string) bool {
	for _, st := range path {
		if st.Action == action {
			return true
		}
	}
	return false
}
