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
	"path/filepath"
	"slices"
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

// specs/project_delete_races.fizz against a real serve: one project per
// walk, the first message minting its main thread, one DELETE, one save
// of MEMORY.md from the project page, an agent's write into the
// directory and a re-create of the same name.
//
// Every interior step of those requests runs inside one HTTP request,
// so the adapter holds each one open with internal/testhold: serve
// parks at a named point while <BOUGH_TEST_HOLD_DIR>/<point> exists and
// writes <point>.reached when it gets there. Init arms every point for
// the walk's slug; each action removes one hold file and waits for the
// next point (or for the request's answer). The points:
//
//	main-checked.<slug>    Main, past its Project() check, holding the lock
//	main-persisted.<slug>  Main, mains and meta written, before the spawn
//	boot.<slug>            the spawned child, before it reads project.yml
//	(llm-control hold_boot) the child, definition read, before its history
//	orb-load.<slug>        the child's orb row, before it reads project.yml
//	image.<slug>           the child's orb row, in EnsureImage before MkdirAll
//	delete-ended.<slug>    DeleteProject, EndProject done
//	delete-emptied.<slug>  the project dir's entries removed, before the rmdir
//	delete-removed.<slug>  dir, images and caches gone, before the meta cleanup
//	save-checked.<slug>.MEMORY.md  putOrbFile, past its project check
//	rename.<slug>.MEMORY.md        atomicWrite, temp file made, before the rename
//
// State is read off the server and the disk: where each request is
// parked (its .reached file with the hold still in place) or what it
// answered; project.yml, the directory, its other files and temp files;
// the image and cache dirs; meta.json's mains and membership; main's
// history and whether serve holds a live child for it. missed is
// derived: a main still in flight (or alive) after the delete ran.
const pdrConfig = "- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type pdrAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	holds string // BOUGH_TEST_HOLD_DIR
	gate  gate

	walk       int
	name, slug string
	mainID     string

	msg, save, dele, create *pdrReq
	agent                   bool

	mains []string // every main that wrote history, for the trace check

	// The deliberate wiring bug the CatchWrongAdapter test injects: the
	// re-create first removes a leftover directory, so it never sees
	// the 409 a zombie directory gives.
	cleanBeforeCreate bool
}

// pdrReq is one request in flight on its own goroutine.
type pdrReq struct {
	done chan struct{}
	code int
	body string
	err  error
}

func (r *pdrReq) answered() bool {
	if r == nil {
		return false
	}
	select {
	case <-r.done:
		return true
	default:
		return false
	}
}

func newPDRAdapter(t *testing.T) *pdrAdapter {
	holds, err := os.MkdirTemp("", "pdr-holds-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(holds) })
	s := servetest.Start(t, servetest.Options{Config: pdrConfig, Env: []string{"BOUGH_TEST_HOLD_DIR=" + holds}})
	return &pdrAdapter{t: t, s: s, dir: control.Dir(s.Home), holds: holds}
}

// The walk's hold points.
func (a *pdrAdapter) hMainChecked() string   { return "main-checked." + a.slug }
func (a *pdrAdapter) hMainPersisted() string { return "main-persisted." + a.slug }
func (a *pdrAdapter) hBoot() string          { return "boot." + a.slug }
func (a *pdrAdapter) hOrbLoad() string       { return "orb-load." + a.slug }
func (a *pdrAdapter) hImage() string         { return "image." + a.slug }
func (a *pdrAdapter) hDelEnded() string      { return "delete-ended." + a.slug }
func (a *pdrAdapter) hDelEmptied() string    { return "delete-emptied." + a.slug }
func (a *pdrAdapter) hDelRemoved() string    { return "delete-removed." + a.slug }
func (a *pdrAdapter) hSaveChecked() string   { return "save-checked." + a.slug + ".MEMORY.md" }
func (a *pdrAdapter) hRename() string        { return "rename." + a.slug + ".MEMORY.md" }

func (a *pdrAdapter) allHolds() []string {
	return []string{a.hMainChecked(), a.hMainPersisted(), a.hBoot(), a.hOrbLoad(), a.hImage(),
		a.hDelEnded(), a.hDelEmptied(), a.hDelRemoved(), a.hSaveChecked(), a.hRename()}
}

func (a *pdrAdapter) exists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

// parked says a process is at point name now: it got there and the
// hold is still in place.
func (a *pdrAdapter) parked(name string) bool {
	return a.exists(filepath.Join(a.holds, name+".reached")) && a.exists(filepath.Join(a.holds, name))
}

func (a *pdrAdapter) release(name string) {
	os.Remove(filepath.Join(a.holds, name))
}

func (a *pdrAdapter) projDir() string {
	return filepath.Join(a.s.Home, ".bough", "projects", a.slug)
}

func (a *pdrAdapter) cacheDirs() []string {
	b := filepath.Join(a.s.Home, ".bough")
	return []string{
		filepath.Join(b, "orbs", "images", a.slug),
		filepath.Join(b, "orbs", "cache", a.slug),
		filepath.Join(b, "cache", a.slug),
	}
}

// Init starts each walk on a fresh project in the same serve, with a
// built image (its image and cache dirs) and every hold armed.
func (a *pdrAdapter) Init() error {
	a.walk++
	a.name = fmt.Sprintf("Race %d", a.walk)
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": a.name}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	for _, d := range a.cacheDirs() {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}
	for _, h := range a.allHolds() {
		if err := os.WriteFile(filepath.Join(a.holds, h), nil, 0o644); err != nil {
			return err
		}
	}
	a.mainID, a.msg, a.save, a.dele, a.create, a.agent = "", nil, nil, nil, nil, false
	a.gate.reset()
	return nil
}

// Cleanup lets every held request and process go, waits for the
// answers, and archives the walk's main so no orphan outlives the walk.
func (a *pdrAdapter) Cleanup() error {
	for _, h := range a.allHolds() {
		a.release(h)
	}
	var errs []error
	deadline := time.Now().Add(actionTimeout)
	for _, r := range []*pdrReq{a.msg, a.save, a.dele, a.create} {
		for r != nil && !r.answered() {
			// A mint let go here boots on to llm-control's hold.
			if id := a.knownMain(); id != "" {
				if _, held := control.Booting(a.dir)[id]; held {
					control.ReleaseBoot(a.t, a.dir, id)
				}
			}
			if time.Now().After(deadline) {
				errs = append(errs, fmt.Errorf("walk %d: a request did not answer after its holds were released", a.walk))
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	if id := a.knownMain(); id != "" && a.hasHistory(id) {
		if err := a.api(http.MethodPost, "/api/sessions/"+id+"/archive", nil, nil); err != nil {
			errs = append(errs, err)
		}
		if !slices.Contains(a.mains, id) {
			a.mains = append(a.mains, id)
		}
	}
	return errors.Join(errs...)
}

func (a *pdrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

func (a *pdrAdapter) meta() (pmtMeta, error) {
	var m pmtMeta
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "serve", "meta.json"))
	if errors.Is(err, os.ErrNotExist) {
		return m, nil
	}
	if err != nil {
		return m, err
	}
	return m, json.Unmarshal(b, &m)
}

// knownMain is the id Main minted for this walk: meta.json's mains
// while it names one, remembered once seen (the delete's cleanup drops
// it from mains).
func (a *pdrAdapter) knownMain() string {
	if a.mainID == "" {
		if m, err := a.meta(); err == nil {
			a.mainID = m.Mains[a.slug]
		}
	}
	return a.mainID
}

func (a *pdrAdapter) hasHistory(id string) bool {
	return a.exists(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
}

// mainState is the spec's main.
func (a *pdrAdapter) mainState() (string, error) {
	if a.msg == nil {
		return "none", nil
	}
	id := a.knownMain()
	if id != "" && a.hasHistory(id) {
		ctx, cancel := actionCtx()
		defer cancel()
		row, _, err := a.s.GetSession(ctx, id)
		if err != nil {
			return "", err
		}
		switch {
		case row.Live && a.parked(a.hOrbLoad()):
			return "live", nil
		case row.Live:
			return "running", nil
		case a.parked(a.hOrbLoad()), a.exists(filepath.Join(a.holds, a.hImage()+".reached")):
			// Gone while parked at its orb row's read, or past it: only
			// a kill stops a child there.
			return "killed", nil
		}
		// Let past its orb row's read and never at its image build: the
		// row failed, and the child exited on its own.
		return "dead", nil
	}
	if id != "" {
		if role, held := control.Booting(a.dir)[id]; held {
			if role != "main" {
				return "", fmt.Errorf("main %s is booting as a %q, not as main", id, role)
			}
			return "booted", nil
		}
	}
	switch {
	case a.parked(a.hBoot()):
		return "spawned", nil
	case a.parked(a.hMainPersisted()):
		return "persisted", nil
	case a.parked(a.hMainChecked()):
		return "checked", nil
	}
	if a.msg.answered() {
		switch a.msg.code {
		case http.StatusNotFound:
			return "none", nil
		case http.StatusInternalServerError:
			return "exited", nil
		}
		return "", fmt.Errorf("message answered %d with main %q neither live nor booting: %s", a.msg.code, id, a.msg.body)
	}
	return "", fmt.Errorf("message in flight and parked nowhere (main %q)", id)
}

func answer(r *pdrReq) string {
	switch {
	case r == nil || !r.answered():
		return ""
	case r.err != nil:
		return "error: " + r.err.Error()
	case r.code == http.StatusOK:
		return "ok"
	}
	return strconv.Itoa(r.code)
}

func (a *pdrAdapter) deleState() (string, error) {
	switch {
	case a.dele == nil:
		return "idle", nil
	case a.dele.answered():
		switch a.dele.code {
		case http.StatusOK:
			return "done", nil
		case http.StatusInternalServerError:
			// The rmdir that found an entry RemoveAll had not read.
			if strings.Contains(a.dele.body, "directory not empty") {
				return "failed", nil
			}
		}
		return "", fmt.Errorf("delete answered %d: %s", a.dele.code, a.dele.body)
	case a.parked(a.hDelRemoved()):
		return "removed", nil
	case a.parked(a.hDelEmptied()):
		return "emptied", nil
	case a.parked(a.hDelEnded()):
		return "ended", nil
	}
	return "", fmt.Errorf("delete in flight and parked nowhere")
}

func (a *pdrAdapter) saveState() (string, error) {
	switch {
	case a.save == nil:
		return "idle", nil
	case a.save.answered():
		switch a.save.code {
		case http.StatusOK:
			return "ok", nil
		case http.StatusNotFound, http.StatusBadRequest:
			return "err", nil
		}
		return "", fmt.Errorf("save answered %d: %s", a.save.code, a.save.body)
	case a.parked(a.hRename()):
		return "tmp", nil
	case a.parked(a.hSaveChecked()):
		return "checked", nil
	}
	return "", fmt.Errorf("save in flight and parked nowhere")
}

// files reads the project directory: project.yml, a temp file of
// atomicWrite's, and anything else.
func (a *pdrAdapter) files() (dir, yml, tmp, stray bool, err error) {
	ents, err := os.ReadDir(a.projDir())
	if errors.Is(err, os.ErrNotExist) {
		return false, false, false, false, nil
	}
	if err != nil {
		return false, false, false, false, err
	}
	for _, e := range ents {
		switch n := e.Name(); {
		case n == "project.yml":
			yml = true
		case strings.HasPrefix(n, ".") && strings.Contains(n, ".tmp-"):
			tmp = true
		default:
			stray = true
		}
	}
	return true, yml, tmp, stray, nil
}

// GetState is the Project role's state.
func (a *pdrAdapter) GetState() (map[string]any, error) {
	main, err := a.mainState()
	if err != nil {
		return nil, err
	}
	dele, err := a.deleState()
	if err != nil {
		return nil, err
	}
	save, err := a.saveState()
	if err != nil {
		return nil, err
	}
	dir, yml, tmp, stray, err := a.files()
	if err != nil {
		return nil, err
	}
	// The page sees the project exactly while project.yml reads.
	err = a.api(http.MethodGet, "/api/projects/"+a.slug, nil, nil)
	if listed := err == nil; listed != yml {
		return nil, fmt.Errorf("GET /api/projects/%s: %v with project.yml there = %v", a.slug, err, yml)
	}
	cache := false
	for _, d := range a.cacheDirs() {
		cache = cache || a.exists(d)
	}
	m, err := a.meta()
	if err != nil {
		return nil, err
	}
	id := a.knownMain()
	filed := id != "" && m.Sessions[id].Project == a.slug
	return map[string]any{
		"yml": yml, "dir": dir, "stray": stray, "tmp": tmp, "cache": cache,
		"main": main, "msg": answer(a.msg),
		"mains": m.Mains[a.slug] != "", "filed": filed,
		"dele":   dele,
		"missed": dele != "idle" && main != "none" && main != "killed",
		"save":   save, "agent": a.agent, "create": answer(a.create),
	}, nil
}

// --- requests

// api is one JSON call; a non-2xx answer is an error.
func (a *pdrAdapter) api(method, path string, body, out any) error {
	code, raw, err := a.call(context.Background(), method, path, body)
	if err != nil {
		return err
	}
	if code/100 != 2 {
		return fmt.Errorf("%s %s: %d %s", method, path, code, raw)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func (a *pdrAdapter) call(ctx context.Context, method, path string, body any) (int, []byte, error) {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, nil, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, nil, fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, raw, nil
}

// send starts a request that may park inside serve.
func (a *pdrAdapter) send(method, path string, body any) *pdrReq {
	r := &pdrReq{done: make(chan struct{})}
	go func() {
		defer close(r.done)
		code, raw, err := a.call(context.Background(), method, path, body)
		r.code, r.body, r.err = code, string(raw), err
	}()
	return r
}

// until waits for ok, or for r to answer (then ok is not required: the
// state says what the answer was).
func (a *pdrAdapter) until(r *pdrReq, what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if r != nil && r.answered() {
			if r.err != nil {
				return r.err
			}
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("walk %d: %s: not after %s", a.walk, what, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

func (a *pdrAdapter) mainIs(states ...string) bool {
	st, err := a.mainState()
	return err == nil && slices.Contains(states, st)
}

func (a *pdrAdapter) deleIs(states ...string) bool {
	st, err := a.deleState()
	return err == nil && slices.Contains(states, st)
}

func (a *pdrAdapter) saveIs(states ...string) bool {
	st, err := a.saveState()
	return err == nil && slices.Contains(states, st)
}

func (a *pdrAdapter) disk() (dir, yml, tmp, stray bool) {
	dir, yml, tmp, stray, _ = a.files()
	return
}

// --- actions: Main(slug)

func (a *pdrAdapter) MessageFirst() error {
	if !a.gate.pass(a.mainIs("none") && a.msg == nil && a.create == nil) {
		return nil
	}
	a.msg = a.send(http.MethodPost, "/api/projects/"+a.slug+"/message", map[string]string{"text": fmt.Sprintf("first message of walk %d", a.walk)})
	return a.until(a.msg, "main past its check", func() bool { return a.parked(a.hMainChecked()) })
}

func (a *pdrAdapter) MainPersist() error {
	if !a.gate.pass(a.mainIs("checked")) {
		return nil
	}
	a.release(a.hMainChecked())
	return a.until(a.msg, "main persisted", func() bool { return a.parked(a.hMainPersisted()) })
}

func (a *pdrAdapter) MainSpawn() error {
	if !a.gate.pass(a.mainIs("persisted")) {
		return nil
	}
	a.knownMain()
	a.release(a.hMainPersisted())
	return a.until(a.msg, "main's child spawned", func() bool { return a.parked(a.hBoot()) })
}

func (a *pdrAdapter) ChildBoot() error {
	if !a.gate.pass(a.mainIs("spawned")) {
		return nil
	}
	id := a.knownMain()
	a.release(a.hBoot())
	// The .waiting file is created before its role is written in it.
	return a.until(a.msg, "main's child booted", func() bool { return control.Booting(a.dir)[id] != "" })
}

// MainHistoryAppears lets the booted child write its history: serve
// claims it and sends the message, and the child's orb row, mounted
// next, parks before it reads project.yml.
func (a *pdrAdapter) MainHistoryAppears() error {
	if !a.gate.pass(a.mainIs("booted")) {
		return nil
	}
	id := a.knownMain()
	control.ReleaseBoot(a.t, a.dir, id)
	if err := a.until(nil, "the message answered", a.msg.answered); err != nil {
		return err
	}
	if a.msg.code != http.StatusOK {
		return nil
	}
	a.mains = append(a.mains, id)
	return a.until(nil, "main's orb row at its read of project.yml", func() bool { return a.parked(a.hOrbLoad()) })
}

// OrbReadsDef lets main's orb row read project.yml. With it there the
// rest of main mounts: its turn on the message runs and closes, and the
// row's image build parks before its MkdirAll. Without it the row fails
// and the child exits.
func (a *pdrAdapter) OrbReadsDef() error {
	if !a.gate.pass(a.mainIs("live")) {
		return nil
	}
	id := a.knownMain()
	_, yml, _, _ := a.disk()
	a.release(a.hOrbLoad())
	if !yml {
		return a.until(nil, "main's child gone", func() bool { return !a.mainIs("live", "running") })
	}
	if err := a.until(nil, "main's turn on the message closed", func() bool {
		es, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
		return err == nil && slices.ContainsFunc(es, func(e history.Entry) bool { return e.Kind == "input" }) && !turnOpen(es)
	}); err != nil {
		return err
	}
	return a.until(nil, "main's orb row at its image build", func() bool { return a.parked(a.hImage()) })
}

// ChildBuildsOrb lets main's orb row make the image dir.
func (a *pdrAdapter) ChildBuildsOrb() error {
	cache := false
	for _, d := range a.cacheDirs() {
		cache = cache || a.exists(d)
	}
	if !a.gate.pass(a.mainIs("running") && !cache) {
		return nil
	}
	a.release(a.hImage())
	return a.until(nil, "the image dir made", func() bool { return a.exists(a.cacheDirs()[0]) })
}

// --- DeleteProject

func (a *pdrAdapter) DeleteProject() error {
	_, yml, _, _ := a.disk()
	if !a.gate.pass(a.dele == nil && yml) {
		return nil
	}
	a.dele = a.send(http.MethodDelete, "/api/projects/"+a.slug, nil)
	return a.until(a.dele, "the delete past EndProject", func() bool { return a.parked(a.hDelEnded()) })
}

func (a *pdrAdapter) RemoveEntries() error {
	if !a.gate.pass(a.deleIs("ended")) {
		return nil
	}
	a.release(a.hDelEnded())
	return a.until(a.dele, "the delete's entries removed", func() bool { return a.parked(a.hDelEmptied()) })
}

func (a *pdrAdapter) RemoveAllOk() error {
	_, _, tmp, stray := a.disk()
	if !a.gate.pass(a.deleIs("emptied") && !tmp && !stray) {
		return nil
	}
	a.release(a.hDelEmptied())
	return a.until(a.dele, "the delete's dirs removed", func() bool { return a.parked(a.hDelRemoved()) })
}

func (a *pdrAdapter) RemoveAllFails() error {
	_, _, tmp, stray := a.disk()
	if !a.gate.pass(a.deleIs("emptied") && (tmp || stray)) {
		return nil
	}
	a.release(a.hDelEmptied())
	return a.until(nil, "the delete answered", a.dele.answered)
}

func (a *pdrAdapter) CleanMeta() error {
	if !a.gate.pass(a.deleIs("removed")) {
		return nil
	}
	a.release(a.hDelRemoved())
	return a.until(nil, "the delete answered", a.dele.answered)
}

// --- PUT orb/files/MEMORY.md

func (a *pdrAdapter) PutOrbFile() error {
	if !a.gate.pass(a.save == nil) {
		return nil
	}
	a.save = a.send(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/MEMORY.md", map[string]string{"text": "a brief\n"})
	return a.until(a.save, "the save past its check", func() bool { return a.parked(a.hSaveChecked()) })
}

func (a *pdrAdapter) SaveTemp() error {
	if !a.gate.pass(a.saveIs("checked")) {
		return nil
	}
	a.release(a.hSaveChecked())
	return a.until(a.save, "the save's temp file", func() bool { return a.parked(a.hRename()) })
}

func (a *pdrAdapter) SaveRename() error {
	if !a.gate.pass(a.saveIs("tmp")) {
		return nil
	}
	a.release(a.hRename())
	return a.until(nil, "the save answered", a.save.answered)
}

// --- an agent's file tools, and the re-create

// AgentWritesFileInDir is what an agent's write tool does with the
// project's MEMORY.md: MkdirAll, then the file.
func (a *pdrAdapter) AgentWritesFileInDir() error {
	if !a.gate.pass(!a.agent && a.deleIs("removed", "done")) {
		return nil
	}
	a.agent = true
	if err := os.MkdirAll(a.projDir(), 0o755); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(a.projDir(), "MEMORY.md"), []byte("remember this\n"), 0o644)
}

func (a *pdrAdapter) CreateProject() error {
	if !a.gate.pass(a.create == nil && a.deleIs("done", "failed")) {
		return nil
	}
	if a.cleanBeforeCreate {
		os.RemoveAll(a.projDir())
	}
	a.create = a.send(http.MethodPost, "/api/projects", map[string]string{"name": a.name})
	return a.until(nil, "the create answered", a.create.answered)
}

// pdrAction logs each step a walk takes while the gate is open: when a
// run fails, the log is the walk.
func pdrAction(name string, f func(*pdrAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*pdrAdapter)
		if a.gate.off {
			return nil, f(a)
		}
		start := time.Now()
		err := f(a)
		if !a.gate.off && (err != nil || testing.Verbose()) {
			a.t.Logf("walk %d: %s (%s) err=%v", a.walk, name, time.Since(start).Round(time.Millisecond), err)
		}
		return nil, err
	}
}

var pdrActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"MessageFirst":         pdrAction("MessageFirst", (*pdrAdapter).MessageFirst),
	"MainPersist":          pdrAction("MainPersist", (*pdrAdapter).MainPersist),
	"MainSpawn":            pdrAction("MainSpawn", (*pdrAdapter).MainSpawn),
	"ChildBoot":            pdrAction("ChildBoot", (*pdrAdapter).ChildBoot),
	"MainHistoryAppears":   pdrAction("MainHistoryAppears", (*pdrAdapter).MainHistoryAppears),
	"OrbReadsDef":          pdrAction("OrbReadsDef", (*pdrAdapter).OrbReadsDef),
	"ChildBuildsOrb":       pdrAction("ChildBuildsOrb", (*pdrAdapter).ChildBuildsOrb),
	"DeleteProject":        pdrAction("DeleteProject", (*pdrAdapter).DeleteProject),
	"RemoveEntries":        pdrAction("RemoveEntries", (*pdrAdapter).RemoveEntries),
	"RemoveAllOk":          pdrAction("RemoveAllOk", (*pdrAdapter).RemoveAllOk),
	"RemoveAllFails":       pdrAction("RemoveAllFails", (*pdrAdapter).RemoveAllFails),
	"CleanMeta":            pdrAction("CleanMeta", (*pdrAdapter).CleanMeta),
	"PutOrbFile":           pdrAction("PutOrbFile", (*pdrAdapter).PutOrbFile),
	"SaveTemp":             pdrAction("SaveTemp", (*pdrAdapter).SaveTemp),
	"SaveRename":           pdrAction("SaveRename", (*pdrAdapter).SaveRename),
	"AgentWritesFileInDir": pdrAction("AgentWritesFileInDir", (*pdrAdapter).AgentWritesFileInDir),
	"CreateProject":        pdrAction("CreateProject", (*pdrAdapter).CreateProject),
}, "": {
	// fizz links a state with nothing enabled to itself as "end", and the
	// runner offers it in every state (deadlock detection is off). Every
	// actor here acts once, so an end is a walk used up: nothing happens
	// in serve, and the gate closes so the walk stops there.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*pdrAdapter).gate.pass(false)
		return nil, nil
	},
}}

// projectDeleteRacesHistory reads main's transcript as a path through
// the spec. A transcript exists only for a main that got past its boot,
// so its meta entry is the whole mint up to its history (MessageFirst
// through MainHistoryAppears, the message answered ok). The child reads
// stdin only once its orb row has read project.yml, so an input there
// is that read having found the definition (OrbReadsDef, running).
// Nothing else in the flow writes to main, so a second input is a
// second read: not a path.
func projectDeleteRacesHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i+1 < len(kv); i += 2 {
			m["Project#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	step := func(action string, kv ...any) tracecheck.Step {
		return tracecheck.Step{Action: "Project#0." + action, State: st(kv...)}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("main", "none", "msg", "")}}
	for _, e := range entries {
		switch e.Kind {
		case "meta":
			steps = append(steps,
				step("MessageFirst", "main", "checked"),
				step("MainPersist", "main", "persisted", "mains", true),
				step("MainSpawn", "main", "spawned"),
				step("ChildBoot", "main", "booted"),
				step("MainHistoryAppears", "main", "live", "msg", "ok"))
		case "input":
			if e.Data["reason"] == "notice" {
				continue
			}
			steps = append(steps, step("OrbReadsDef", "main", "running"))
		}
	}
	return steps
}

func init() { historyProjections["project_delete_races"] = projectDeleteRacesHistory }

func pdrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 12, "max-parallel-runs": 0}
}

func TestProjectDeleteRaces(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPDRAdapter(t)
	if err := runMBT(t, "project_delete_races", a, pdrActions, pdrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "project_delete_races"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.mains {
		if a.hasHistory(id) {
			checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectDeleteRacesHistory)
		}
	}
}

// walkPath drives one generated path through the adapter and compares
// the state it reads off serve with the path's after every step.
func (a *pdrAdapter) walkPath(trace []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	var done []string
	for i, s := range trace {
		name := strings.TrimPrefix(s.Action, "Project#0.")
		if i > 0 && name == "end" {
			// A state with nothing left to do, linked to itself: the
			// state is checked again below, with nothing done in serve.
			done = append(done, name)
		} else if i > 0 {
			done = append(done, name)
			f, ok := pdrActions["Project"][name]
			if !ok {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s) of %v: %w", i, name, done, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s) of %v: the adapter reads it as not enabled", i, name, done)
			}
		}
		have, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s) of %v: state: %w", i, name, done, err)
		}
		if diff := pmtDiff(s.State, have); diff != "" {
			return fmt.Errorf("step %d (%s) of %v: state differs from the spec's:%s", i, name, done, diff)
		}
	}
	return nil
}

func pdrPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("project_delete_races", cover)
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range file.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// TestProjectDeleteRacesPaths walks every generated path (every settled
// state; MODEL_COVER=transitions: every transition) through the server
// adapter, then replays every transcript main wrote on the graph. Four
// serves share the paths.
func TestProjectDeleteRacesPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := pdrPaths(t, envCover())
	g, err := tracecheck.Load(fizzCheck(t, "project_delete_races"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPDRAdapter(t)
			failed := 0
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
					if failed++; failed >= 5 {
						t.Fatal("five paths failed on this serve; stopping")
					}
				}
			}
			checked := 0
			for _, id := range a.mains {
				if a.hasHistory(id) {
					checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectDeleteRacesHistory)
					checked++
				}
			}
			if checked == 0 {
				t.Error("no transcripts for the trace check")
			}
			t.Logf("walked %d paths, trace-checked %d transcripts", (len(paths)-n+shards-1)/shards, checked)
		})
	}
}

// A re-create that first removes a leftover directory never sees the
// 409 a zombie directory gives: the walk over every transition must say
// so. It stops at the first path that does.
func TestProjectDeleteRacesPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPDRAdapter(t)
	a.cleanBeforeCreate = true
	for i, p := range pdrPaths(t, tracecheck.CoverTransitions) {
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with a re-create that removes the leftover directory; the walk is not checking state")
}
