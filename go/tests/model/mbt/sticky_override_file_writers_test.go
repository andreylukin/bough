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
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"
	"gopkg.in/yaml.v3"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/sticky_override_file_writers.fizz against a real serve: the
// Page role is the web page (a Context page's switch for skill "a", the
// composer's skill catalogue, a Me row), played by the adapter the way
// the page's code does, over one serve with one session whose child is
// the "session child" of the caches scene.
//
// What serve or the disk owns is read back every step: off.yml's bytes
// (disk_a, bad, comment, and b against what the other tab last set),
// serve's own offlist cache through GET /api/sessions/{id}/context
// (serve_stale), and triage.json through GET /api/me (mk, and j against
// what the other tab last set). The page's own state (overrides, the
// last poll, a request in flight, the catalogue it cached) is kept here
// from the answers serve gives. The child's cache and its registered
// commands cannot be read without sending, so child_stale and reg_a are
// kept here and checked by what a "/a" send does (sent, read off the
// session's history).
//
// The spec splits two requests at a read-modify-write that the real
// handlers do in one breath (offlist.Set, wiki.Mark): the other tab's
// write lands between the page's read and its write. The adapter makes
// that interleaving real by running the page's request first (it reads
// the file as it was at the page's click), putting back the bytes it
// read, running the other tab's request, and landing the bytes the
// page's request wrote at the spec's answer: every byte on disk is one
// the product computed; only when the page's write reaches the disk is
// the adapter's.
//
// SameStampRewrite needs both caches to hold the current file: before
// it the adapter makes serve read it (a context GET) and the child read
// it ("/rules", a command that consults off.yml), then rewrites the file
// with a flipped at the same size (both versions padded to one length
// with a comment, the first written before the caches read it) and puts
// its mtime back.

const (
	// Skill names; bravo is only ever an off.yml entry.
	sofwA    = "alpha"
	sofwB    = "bravo"
	sofwOn   = "charlie" // a skill always on: the catalogue is never empty
	sofwK    = "gh:acme/web#7"
	sofwJ    = "gh:acme/web#8"
	sofwRule = "Nothing from acme/web"

	sofwInitOff = "disabled: []\n"
	sofwComment = "# why: kept by hand\n"
	sofwBroken  = "broken: [\n"
)

type sofwAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	id   string // the session: its context page and its child
	gate gate
	turn int

	// The Page role's page-side fields, as the spec names them.
	scene                     string
	bMoved, err, wrote        bool
	polled                    bool
	wireA                     bool
	ovrA, req                 string
	childStale, regA          bool
	cat, picker, sent         string
	jMoved, merr, mpolled     bool
	mwire, movr, mreq         string
	commentAdded, bWant, jWnt bool

	// The page's request the spec holds in flight: the bytes off.yml (or
	// triage.json) had at the click, and, once the other tab raced it,
	// what the page's request wrote and answered.
	before   []byte
	raced    bool
	landed   []byte
	rWrote   bool
	rErr     error
	rTriage  string
	walkFrom int // history entries before this walk: its slice of the transcript
	walks    [][2]int
	// bootRefused counts "/rules" lines a mounting child refused.
	bootRefused int

	// wrongID is the deliberate bug the CatchesWrongAdapter tests inject:
	// the switch posts a hook's id, so off.yml never follows the click.
	wrongID bool
}

func newSofwAdapter(t *testing.T) *sofwAdapter {
	skill := func(name string) string {
		return "---\ndescription: The " + name + " fixture skill.\n---\nfixture body of " + name + "\n"
	}
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Files: map[string]string{
		".claude/skills/" + sofwA + "/SKILL.md":  skill(sofwA),
		".claude/skills/" + sofwOn + "/SKILL.md": skill(sofwOn),
		".bough/off.yml":                         sofwInitOff,
	}})
	a := &sofwAdapter{t: t, s: s, dir: control.Dir(s.Home), regA: true}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.id = row.ID
	// The child registers its commands at mount; wait for it to answer a
	// command so the first walk's Init starts from a mounted child.
	if err := a.rules(); err != nil {
		t.Fatal(err)
	}
	return a
}

func (a *sofwAdapter) offYML() string { return filepath.Join(a.s.Home, ".bough", "off.yml") }
func (a *sofwAdapter) triageJS() string {
	return filepath.Join(a.s.Home, ".bough", "wiki", "topics", "me", "triage.json")
}
func (a *sofwAdapter) histPath() string {
	return filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")
}

// Init is a fresh page: off.yml with nothing of ours off, no triage, and
// a child that registered "/a" (restarted when the last walk left one
// that mounted with a off).
func (a *sofwAdapter) Init() error {
	a.endWalk()
	if err := os.WriteFile(a.offYML(), []byte(sofwInitOff), 0o644); err != nil {
		return err
	}
	if err := os.Remove(a.triageJS()); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if !a.regA {
		if err := a.restart(); err != nil {
			return err
		}
	}
	a.scene = "start"
	a.bMoved, a.err, a.wrote, a.polled, a.wireA = false, false, false, false, false
	a.ovrA, a.req = "none", "none"
	a.childStale, a.regA = false, true
	a.cat, a.picker, a.sent = "unloaded", "closed", ""
	a.jMoved, a.merr, a.mpolled = false, false, false
	a.mwire, a.movr, a.mreq = "shown", "none", "none"
	a.commentAdded, a.bWant, a.jWnt = false, false, false
	a.before, a.raced, a.landed = nil, false, nil
	entries, err := history.Read(a.histPath())
	if err != nil {
		return err
	}
	a.walkFrom = len(entries)
	a.gate.reset()
	return nil
}

// endWalk records the transcript slice the walk that just ended wrote.
func (a *sofwAdapter) endWalk() {
	if a.scene != "caches" {
		return
	}
	entries, err := history.Read(a.histPath())
	if err == nil && len(entries) > a.walkFrom {
		a.walks = append(a.walks, [2]int{a.walkFrom, len(entries)})
	}
	a.scene = ""
}

// Cleanup has nothing to release: no turn is ever held.
func (a *sofwAdapter) Cleanup() error { return nil }

func (a *sofwAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// offFile is off.yml as the disk has it.
type offFile struct {
	raw           []byte
	a, b, bad     bool
	comment, have bool
}

var sofwEntry = func(id string) *regexp.Regexp {
	return regexp.MustCompile(`(?m)^\s*-\s*skill:` + id + `\s*$`)
}

var sofwEntryA, sofwEntryB = sofwEntry(sofwA), sofwEntry(sofwB)

func (a *sofwAdapter) readOff() (offFile, error) {
	b, err := os.ReadFile(a.offYML())
	if errors.Is(err, os.ErrNotExist) {
		return offFile{}, nil
	}
	if err != nil {
		return offFile{}, err
	}
	var f struct {
		Disabled []string `yaml:"disabled"`
		Off      []string `yaml:"off"`
	}
	return offFile{
		raw: b, have: true,
		a: sofwEntryA.Match(b), b: sofwEntryB.Match(b),
		bad:     yaml.Unmarshal(b, &f) != nil,
		comment: bytes.Contains(b, []byte(sofwComment)),
	}, nil
}

// serveOff is skill a's switch as serve reads off.yml (through its
// cache): the Context page's poll.
func (a *sofwAdapter) serveOff() (bool, error) {
	var d struct {
		Skills []struct {
			ID  string `json:"id"`
			Off bool   `json:"off"`
		} `json:"skills"`
	}
	if err := a.call(http.MethodGet, "/api/sessions/"+url.PathEscape(a.id)+"/context", nil, &d); err != nil {
		return false, err
	}
	for _, s := range d.Skills {
		if s.ID == sofwA {
			return s.Off, nil
		}
	}
	return false, fmt.Errorf("the session's context does not list skill %s", sofwA)
}

type sofwTriage struct {
	Dismissed map[string]string `json:"dismissed"`
	Pinned    []string          `json:"pinned"`
}

// of is a key's row as the page shows it.
func (t sofwTriage) of(key string) string {
	if t.Dismissed[key] != "" {
		return "dismissed"
	}
	for _, p := range t.Pinned {
		if p == key {
			return "pinned"
		}
	}
	return "shown"
}

func (a *sofwAdapter) me() (sofwTriage, error) {
	var d struct {
		Triage sofwTriage `json:"triage"`
	}
	err := a.call(http.MethodGet, "/api/me", nil, &d)
	return d.Triage, err
}

func (a *sofwAdapter) GetState() (map[string]any, error) {
	f, err := a.readOff()
	if err != nil {
		return nil, err
	}
	off, err := a.serveOff()
	if err != nil {
		return nil, err
	}
	tr, err := a.me()
	if err != nil {
		return nil, err
	}
	comment := "none"
	if a.commentAdded {
		comment = "lost"
		if f.comment {
			comment = "kept"
		}
	}
	return map[string]any{
		"scene": a.scene,
		// toggle
		"disk_a": f.a, "b_moved": a.bMoved, "lost_b": f.b != a.bWant, "bad": f.bad,
		"comment": comment, "wire_a": a.wireA, "ovr_a": a.ovrA, "req": a.req,
		"err": a.err, "wrote": a.wrote, "polled": a.polled,
		// caches: serve's read of a against what the file says
		"serve_stale": off != (f.a && !f.bad), "child_stale": a.childStale, "reg_a": a.regA,
		"cat": a.cat, "picker": a.picker, "sent": a.sent,
		// triage
		"mk": tr.of(sofwK), "j_moved": a.jMoved, "lost_j": (tr.of(sofwJ) == "pinned") != a.jWnt,
		"mwire": a.mwire, "movr": a.movr, "mreq": a.mreq, "merr": a.merr, "mpolled": a.mpolled,
	}, nil
}

// --- scenes -------------------------------------------------------------

func (a *sofwAdapter) StartToggle() error {
	if a.gate.pass(a.scene == "start") {
		a.scene, a.polled = "toggle", true
	}
	return nil
}

func (a *sofwAdapter) StartCaches() error {
	if a.gate.pass(a.scene == "start") {
		a.scene = "caches"
	}
	return nil
}

func (a *sofwAdapter) StartTriage() error {
	if a.gate.pass(a.scene == "start") {
		a.scene, a.mpolled = "triage", true
	}
	return nil
}

// --- toggle ---------------------------------------------------------------

func (a *sofwAdapter) shownA() bool {
	if a.ovrA == "none" {
		return a.wireA
	}
	return a.ovrA == "off"
}

func (a *sofwAdapter) HooksPoll() error {
	if !a.gate.pass(a.scene == "toggle" && a.req == "none" && !a.polled) {
		return nil
	}
	off, err := a.serveOff()
	if err != nil {
		return err
	}
	a.wireA, a.polled = off, true
	return nil
}

// Toggle is the click; the request goes out at Answer, or at the other
// tab's race. req is what the page's request will find, from the file
// as the click leaves it: Answer's wrote and err check it against what
// the request did.
func (a *sofwAdapter) Toggle() error {
	if !a.gate.pass(a.scene == "toggle" && a.req == "none") {
		return nil
	}
	f, err := a.readOff()
	if err != nil {
		return err
	}
	a.err = false
	switch {
	case f.bad:
		a.req = "refused"
	case f.a != a.shownA():
		a.req = "noop"
	default:
		a.req = "write"
	}
	a.before, a.raced = f.raw, false
	return nil
}

// setA is the page's POST /api/off for a: whether it wrote off.yml, and
// what it answered.
func (a *sofwAdapter) setA(off bool) (wrote bool, err error) {
	st0, _ := os.Stat(a.offYML())
	b0, _ := os.ReadFile(a.offYML())
	id := "skill:" + sofwA
	if a.wrongID {
		id = "hook:" + sofwA
	}
	err = a.call(http.MethodPost, "/api/off", map[string]any{"id": id, "off": off}, nil)
	st1, _ := os.Stat(a.offYML())
	b1, _ := os.ReadFile(a.offYML())
	wrote = !bytes.Equal(b0, b1) || st0 == nil || st1 == nil || !st0.ModTime().Equal(st1.ModTime())
	return wrote, err
}

func (a *sofwAdapter) Answer() error {
	if !a.gate.pass(a.scene == "toggle" && a.req != "none") {
		return nil
	}
	want := !a.shownA()
	wrote, err := a.rWrote, a.rErr
	if a.raced {
		// The page's request ran at the race; its write lands now.
		if err := os.WriteFile(a.offYML(), a.landed, 0o644); err != nil {
			return err
		}
	} else {
		wrote, err = a.setA(want)
	}
	var api *servetest.APIError
	if err != nil && !errors.As(err, &api) {
		return err
	}
	a.wrote = wrote
	if err != nil {
		a.err = true
	} else {
		if wrote {
			a.polled = false
		}
		a.ovrA = "on"
		if want {
			a.ovrA = "off"
		}
	}
	a.req, a.bMoved, a.raced = "none", false, false
	return nil
}

// setB is the other tab's POST /api/off for b.
func (a *sofwAdapter) setB(off bool) error {
	return a.call(http.MethodPost, "/api/off", map[string]any{"id": "skill:" + sofwB, "off": off}, nil)
}

func (a *sofwAdapter) OtherTabToggleA() error {
	f, err := a.readOff()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.scene == "toggle" && a.req == "none" && !f.bad) {
		return nil
	}
	if err := a.call(http.MethodPost, "/api/off", map[string]any{"id": "skill:" + sofwA, "off": !f.a}, nil); err != nil {
		return err
	}
	a.polled = false
	return nil
}

func (a *sofwAdapter) OtherTabToggleB() error {
	f, err := a.readOff()
	if err != nil {
		return err
	}
	lost := f.b != a.bWant
	if !a.gate.pass(a.scene == "toggle" && !f.bad && ((a.req == "write" && !a.bMoved) || (a.req == "none" && lost))) {
		return nil
	}
	if a.req == "write" {
		// The page's Set read the file at the click: run it on those
		// bytes, keep what it wrote, and put them back for the other tab.
		a.rWrote, a.rErr = a.setA(!a.shownA())
		if a.rErr != nil {
			return fmt.Errorf("the page's Set, raced: %w", a.rErr)
		}
		if a.landed, err = os.ReadFile(a.offYML()); err != nil {
			return err
		}
		if err := os.WriteFile(a.offYML(), a.before, 0o644); err != nil {
			return err
		}
		// The other tab flips b, or, when a write already lost its b,
		// puts back the b it wants.
		a.raced, a.bMoved = true, true
		if !lost {
			a.bWant = !a.bWant
		}
	}
	return a.setB(a.bWant)
}

func (a *sofwAdapter) HandComment() error {
	f, err := a.readOff()
	if err != nil {
		return err
	}
	comment := a.commentAdded && f.comment
	if !a.gate.pass(a.scene == "toggle" && a.req == "none" && !comment && !f.bad) {
		return nil
	}
	a.commentAdded = true
	return os.WriteFile(a.offYML(), append([]byte(sofwComment), f.raw...), 0o644)
}

func (a *sofwAdapter) HandBreak() error {
	f, err := a.readOff()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.scene == "toggle" && a.req == "none" && !f.bad) {
		return nil
	}
	a.polled = false
	return os.WriteFile(a.offYML(), append(f.raw, sofwBroken...), 0o644)
}

func (a *sofwAdapter) HandFix() error {
	f, err := a.readOff()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.scene == "toggle" && a.req == "none" && f.bad) {
		return nil
	}
	a.polled = false
	return os.WriteFile(a.offYML(), bytes.Replace(f.raw, []byte(sofwBroken), nil, 1), 0o644)
}

// LeaveAndReturn remounts the page's view: its override and error are
// gone, and it reads again on mount.
func (a *sofwAdapter) LeaveAndReturn() error {
	if !a.gate.pass((a.scene == "toggle" || a.scene == "triage") && a.req == "none" && a.mreq == "none") {
		return nil
	}
	if a.scene == "toggle" {
		off, err := a.serveOff()
		if err != nil {
			return err
		}
		a.ovrA, a.err, a.wireA, a.polled = "none", false, off, true
		return nil
	}
	tr, err := a.me()
	if err != nil {
		return err
	}
	a.movr, a.merr, a.mwire, a.mpolled = "none", false, tr.of(sofwK), true
	return nil
}

// --- caches -------------------------------------------------------------

func (a *sofwAdapter) SetA() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "closed") {
		return nil
	}
	f, err := a.readOff()
	if err != nil {
		return err
	}
	if _, err := a.setA(!f.a); err != nil {
		return err
	}
	a.childStale, a.sent = false, ""
	return nil
}

func (a *sofwAdapter) SameStampRewrite() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "closed" && !a.childStale) {
		return nil
	}
	f, err := a.readOff()
	if err != nil {
		return err
	}
	var list struct {
		Disabled []string `yaml:"disabled"`
	}
	if err := yaml.Unmarshal(f.raw, &list); err != nil {
		return err
	}
	var flipped []string
	for _, e := range list.Disabled {
		if e != "skill:"+sofwA {
			flipped = append(flipped, e)
		}
	}
	if !f.a {
		flipped = append(flipped, "skill:"+sofwA)
	}
	// The same entries and the flipped ones, padded with a comment to
	// one size: a hand edit that changes no entry, then the rewrite.
	cur, next := sofwOffText(list.Disabled), sofwOffText(flipped)
	n := max(len(cur), len(next)) + 2
	pad := func(b string) []byte { return []byte(b + "#" + strings.Repeat("-", n-len(b)-2) + "\n") }
	if err := os.WriteFile(a.offYML(), pad(cur), 0o644); err != nil {
		return err
	}
	// Both caches read the file as it is now...
	if _, err := a.serveOff(); err != nil {
		return err
	}
	if err := a.rules(); err != nil {
		return err
	}
	st, err := os.Stat(a.offYML())
	if err != nil {
		return err
	}
	// ...and it changes under the same size and mtime.
	if err := os.WriteFile(a.offYML(), pad(next), 0o644); err != nil {
		return err
	}
	if err := os.Chtimes(a.offYML(), time.Now(), st.ModTime()); err != nil {
		return err
	}
	a.childStale, a.sent = true, ""
	return nil
}

// sofwOffText is off.yml listing entries.
func sofwOffText(entries []string) string {
	s := "disabled:\n"
	for _, e := range entries {
		s += "    - " + e + "\n"
	}
	return s
}

func (a *sofwAdapter) OpenPicker() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "closed") {
		return nil
	}
	if a.cat == "unloaded" {
		var d struct {
			Skills []struct {
				ID string `json:"id"`
			} `json:"skills"`
		}
		if err := a.call(http.MethodGet, "/api/skills?session="+url.QueryEscape(a.id), nil, &d); err != nil {
			return err
		}
		a.cat = "omits"
		for _, s := range d.Skills {
			if s.ID == sofwA {
				a.cat = "offers"
			}
		}
		if len(d.Skills) == 0 {
			return fmt.Errorf("GET /api/skills answered an empty catalogue")
		}
	}
	a.picker = a.cat
	return nil
}

func (a *sofwAdapter) ClosePicker() error {
	if a.gate.pass(a.scene == "caches" && a.picker != "closed") {
		a.picker = "closed"
	}
	return nil
}

func (a *sofwAdapter) PickSend() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "offers") {
		return nil
	}
	a.picker = "closed"
	return a.send()
}

func (a *sofwAdapter) TypeSend() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "closed") {
		return nil
	}
	return a.send()
}

func (a *sofwAdapter) Reload() error {
	if a.gate.pass(a.scene == "caches" && a.picker == "closed") {
		a.cat = "unloaded"
	}
	return nil
}

func (a *sofwAdapter) RestartChild() error {
	if !a.gate.pass(a.scene == "caches" && a.picker == "closed") {
		return nil
	}
	if err := a.restart(); err != nil {
		return err
	}
	f, err := a.readOff()
	if err != nil {
		return err
	}
	a.regA, a.childStale, a.sent = !f.a, false, ""
	return nil
}

// restart ends the session's child (archive kills it) and starts a
// fresh one, which mounts now: the resume a serve restart makes.
func (a *sofwAdapter) restart() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the archived child to exit", func(r serve.Row) bool { return !r.Live }); err != nil {
		return err
	}
	if _, err := a.s.Unarchive(ctx, a.id); err != nil {
		return err
	}
	return a.rules()
}

// rules sends "/rules", a command that runs no turn and reads off.yml
// through the child's cache, and waits for the child's answer in the
// history. A child that is still mounting its rows answers "unknown
// command" for a moment (the rows register their commands as they
// mount): the adapter sends again until the command is there, and
// counts how often that happened.
func (a *sofwAdapter) rules() error {
	for range 100 {
		got, err := a.submit("/rules", func(es []history.Entry) (string, bool) {
			for _, e := range es {
				if e.Kind == "system" {
					text, _ := e.Data["text"].(string)
					return text, true
				}
			}
			return "", false
		})
		if err != nil {
			return err
		}
		if !strings.Contains(got, "unknown command") {
			return nil
		}
		a.bootRefused++
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("the child never registered /rules")
}

// send is "/a" and Send: the child refuses a command it never
// registered, or runs the turn with or without the skill's body.
func (a *sofwAdapter) send() error {
	a.turn++
	name := fmt.Sprintf("sofw%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "answered " + name})
	got, err := a.submit("/"+sofwA, func(es []history.Entry) (string, bool) {
		ran, input := false, false
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			switch e.Kind {
			case "system":
				if strings.Contains(text, "unknown command") {
					return "unknown", true
				}
			case "input":
				input = true
				ran = strings.Contains(text, "[skill: "+sofwA+"]")
			case "done", "error":
				if input {
					if ran {
						return "ran", true
					}
					return "plain", true
				}
			}
		}
		return "", false
	})
	if err != nil {
		return err
	}
	if got == "unknown" {
		if err := os.Remove(filepath.Join(a.dir, name+".json")); err != nil {
			return fmt.Errorf("an unknown command took the queued turn %s: %w", name, err)
		}
	}
	a.sent = got
	return nil
}

// submit sends line to the session and waits until the history written
// after it answers.
func (a *sofwAdapter) submit(line string, answered func([]history.Entry) (string, bool)) (string, error) {
	entries, err := history.Read(a.histPath())
	if err != nil {
		return "", err
	}
	from := len(entries)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, line); err != nil {
		return "", err
	}
	for {
		if entries, err := history.Read(a.histPath()); err == nil && len(entries) > from {
			if got, ok := answered(entries[from:]); ok {
				return got, nil
			}
		}
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("%s: no answer in the history: %v", line, ctx.Err())
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// --- triage ---------------------------------------------------------------

func (a *sofwAdapter) shownK() string {
	if a.movr == "none" {
		return a.mwire
	}
	return a.movr
}

// triage is one POST /api/me/triage: the answer's row for k, or the
// error the page toasts.
func (a *sofwAdapter) triage(action, key, rule string) (string, error) {
	var ans struct {
		Triage sofwTriage `json:"triage"`
	}
	err := a.call(http.MethodPost, "/api/me/triage", map[string]string{"action": action, "key": key, "rule": rule}, &ans)
	return ans.Triage.of(sofwK), err
}

func (a *sofwAdapter) MePoll() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && !a.mpolled) {
		return nil
	}
	tr, err := a.me()
	if err != nil {
		return err
	}
	a.mwire, a.mpolled = tr.of(sofwK), true
	return nil
}

func (a *sofwAdapter) PinSend() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && a.shownK() == "shown") {
		return nil
	}
	b, err := os.ReadFile(a.triageJS())
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	a.before, a.raced = b, false
	a.mreq, a.merr = "pin", false
	return nil
}

func (a *sofwAdapter) PinAnswer() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "pin") {
		return nil
	}
	got, err := a.rTriage, a.rErr
	if a.raced {
		if err := os.WriteFile(a.triageJS(), a.landed, 0o644); err != nil {
			return err
		}
	} else {
		got, err = a.triage("pin", sofwK, "")
	}
	if err != nil {
		return err
	}
	a.movr, a.mreq, a.mpolled, a.jMoved, a.raced = got, "none", false, false, false
	return nil
}

func (a *sofwAdapter) Unpin() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && a.shownK() == "pinned") {
		return nil
	}
	got, err := a.triage("unpin", sofwK, "")
	if err != nil {
		return err
	}
	a.movr, a.merr, a.mpolled = got, false, false
	return nil
}

func (a *sofwAdapter) Dismiss() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && (a.shownK() == "shown" || a.shownK() == "pinned")) {
		return nil
	}
	got, err := a.triage("dismiss", sofwK, "")
	if err != nil {
		return err
	}
	a.movr, a.merr, a.mpolled = got, false, false
	return nil
}

// DismissRule is "Nothing from <repo>" on a wiki with no profile.md:
// serve answers 409, the page toasts and keeps its override.
func (a *sofwAdapter) DismissRule() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && (a.shownK() == "shown" || a.shownK() == "pinned")) {
		return nil
	}
	_, err := a.triage("dismiss", sofwK, sofwRule)
	var api *servetest.APIError
	if !errors.As(err, &api) {
		return fmt.Errorf("dismiss with a rule and no profile: %v, want a refusal", err)
	}
	a.merr, a.mpolled = true, false
	return nil
}

// PinDismissReordered: serve answers Pin, then Dismiss; the page gets
// Dismiss's answer first and Pin's last, and keeps Pin's.
func (a *sofwAdapter) PinDismissReordered() error {
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && a.shownK() == "shown") {
		return nil
	}
	pin, err := a.triage("pin", sofwK, "")
	if err != nil {
		return err
	}
	if _, err := a.triage("dismiss", sofwK, ""); err != nil {
		return err
	}
	a.movr, a.merr, a.mpolled = pin, false, false
	return nil
}

func (a *sofwAdapter) OtherTabUndismiss() error {
	tr, err := a.me()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.scene == "triage" && a.mreq == "none" && tr.of(sofwK) == "dismissed") {
		return nil
	}
	if _, err := a.triage("undismiss", sofwK, ""); err != nil {
		return err
	}
	a.mpolled = false
	return nil
}

// pinJ is the other tab's pin or unpin of j, as it wants it now.
func (a *sofwAdapter) pinJ() error {
	action := "unpin"
	if a.jWnt {
		action = "pin"
	}
	_, err := a.triage(action, sofwJ, "")
	return err
}

func (a *sofwAdapter) OtherTabPinJ() error {
	tr, err := a.me()
	if err != nil {
		return err
	}
	lost := (tr.of(sofwJ) == "pinned") != a.jWnt
	if !a.gate.pass(a.scene == "triage" && ((a.mreq == "pin" && !a.jMoved) || (a.mreq == "none" && lost))) {
		return nil
	}
	if a.mreq == "pin" {
		// As with the switch: the page's Mark runs on the bytes it read,
		// they go back, the other tab writes, and the page's lands later.
		a.rTriage, a.rErr = a.triage("pin", sofwK, "")
		if a.rErr != nil {
			return fmt.Errorf("the page's Mark, raced: %w", a.rErr)
		}
		if a.landed, err = os.ReadFile(a.triageJS()); err != nil {
			return err
		}
		if a.before == nil {
			err = os.Remove(a.triageJS())
		} else {
			err = os.WriteFile(a.triageJS(), a.before, 0o644)
		}
		if err != nil {
			return err
		}
		a.raced, a.jMoved = true, true
		if !lost {
			a.jWnt = !a.jWnt
		}
	}
	return a.pinJ()
}

// --- plumbing -------------------------------------------------------------

func (a *sofwAdapter) call(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return sofwCall(ctx, a.s, method, path, body, out)
}

func sofwCall(ctx context.Context, s *servetest.Server, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

var sofwActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"StartToggle":         action((*sofwAdapter).StartToggle),
	"StartCaches":         action((*sofwAdapter).StartCaches),
	"StartTriage":         action((*sofwAdapter).StartTriage),
	"HooksPoll":           action((*sofwAdapter).HooksPoll),
	"Toggle":              action((*sofwAdapter).Toggle),
	"Answer":              action((*sofwAdapter).Answer),
	"OtherTabToggleA":     action((*sofwAdapter).OtherTabToggleA),
	"OtherTabToggleB":     action((*sofwAdapter).OtherTabToggleB),
	"HandComment":         action((*sofwAdapter).HandComment),
	"HandBreak":           action((*sofwAdapter).HandBreak),
	"HandFix":             action((*sofwAdapter).HandFix),
	"LeaveAndReturn":      action((*sofwAdapter).LeaveAndReturn),
	"SetA":                action((*sofwAdapter).SetA),
	"SameStampRewrite":    action((*sofwAdapter).SameStampRewrite),
	"OpenPicker":          action((*sofwAdapter).OpenPicker),
	"ClosePicker":         action((*sofwAdapter).ClosePicker),
	"PickSend":            action((*sofwAdapter).PickSend),
	"TypeSend":            action((*sofwAdapter).TypeSend),
	"Reload":              action((*sofwAdapter).Reload),
	"RestartChild":        action((*sofwAdapter).RestartChild),
	"MePoll":              action((*sofwAdapter).MePoll),
	"PinSend":             action((*sofwAdapter).PinSend),
	"PinAnswer":           action((*sofwAdapter).PinAnswer),
	"Unpin":               action((*sofwAdapter).Unpin),
	"Dismiss":             action((*sofwAdapter).Dismiss),
	"DismissRule":         action((*sofwAdapter).DismissRule),
	"PinDismissReordered": action((*sofwAdapter).PinDismissReordered),
	"OtherTabUndismiss":   action((*sofwAdapter).OtherTabUndismiss),
	"OtherTabPinJ":        action((*sofwAdapter).OtherTabPinJ),
}}

// The runner picks from all 29 actions and stops checking a walk at the
// first disabled one; a walk is three scenes deep at most before it does
// anything, so run many short ones.
func sofwOptions() map[string]any {
	return map[string]any{"max-seq-runs": 400, "max-actions": 6, "max-parallel-runs": 0}
}

// sofwHistory reads one caches walk's slice of the session transcript.
// History records a "/a" send (a command entry, then "unknown command",
// or a turn whose input does or does not carry the skill's body) and a
// fresh child: the engine writes an "engine" entry at the first turn a
// process runs. It does not record what the page did to off.yml, so the
// projection puts in the fewest SetA hops that make each send's outcome
// reachable, and a RestartChild only where the transcript allows one:
// before a refusal from a child that had registered "/a" (a child that
// refuses runs no turn, so it leaves no entry of its own), or at a turn
// with an "engine" entry. What is left to fail is a child that refused
// "/a" and later ran it with no fresh process in between.
func sofwHistory(entries []history.Entry) []tracecheck.Step {
	sent := func(v string) map[string]any { return map[string]any{"Page#0.sent": v} }
	steps := []tracecheck.Step{{Action: "Init", State: sent("")}, {Action: "Page#0.StartCaches", State: sent("")}}
	disk, reg := false, true
	hop := func(name string) { steps = append(steps, tracecheck.Step{Action: "Page#0." + name}) }
	setA := func(want bool) {
		if disk != want {
			disk = want
			hop("SetA")
		}
	}
	fresh := func(on bool) {
		setA(!on)
		hop("RestartChild")
		reg = on
	}
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		if e.Kind != "command" || strings.TrimSpace(text) != "/"+sofwA {
			continue
		}
		// The command's answer, and the turn it started, if any.
		outcome, engine := "", false
	answer:
		for _, f := range entries[i+1:] {
			ft, _ := f.Data["text"].(string)
			switch f.Kind {
			case "command":
				break answer
			case "system":
				if strings.Contains(ft, "unknown command") {
					outcome = "unknown"
					break answer
				}
			case "input":
				outcome = "plain"
				if strings.Contains(ft, "[skill: "+sofwA+"]") {
					outcome = "ran"
				}
			case "engine":
				engine = true
			}
		}
		switch outcome {
		case "unknown":
			if reg {
				fresh(false)
			}
		case "ran", "plain":
			if engine && !reg {
				fresh(true)
			}
			setA(outcome == "plain")
		default:
			continue
		}
		steps = append(steps, tracecheck.Step{Action: "Page#0.TypeSend", State: sent(outcome)})
	}
	return steps
}

func init() { historyProjections["sticky_override_file_writers"] = sofwHistory }

// walkSofwPaths walks the spec's paths over the checked-in graph against
// the serve, comparing every field after every step.
func walkSofwPaths(t *testing.T, a *sofwAdapter, cover tracecheck.Cover) []string {
	t.Helper()
	b, err := pathsJSONCover("sticky_override_file_writers", cover)
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
		t.Fatal("no paths")
	}
	var bad []string
	for i, p := range out.Paths {
	steps:
		for j, step := range p.Trace {
			if j == 0 {
				err = a.Init()
			} else {
				f, ok := sofwActions["Page"][strings.TrimPrefix(step.Action, "Page#0.")]
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
				field, ok := strings.CutPrefix(k, "Page#0.")
				if ok && fmt.Sprint(got[field]) != fmt.Sprint(want) {
					bad = append(bad, fmt.Sprintf("path %d step %d (%s): %s is %v, the spec says %v", i, j, step.Action, field, got[field], want))
					break steps
				}
			}
		}
	}
	a.endWalk()
	return bad
}

// checkSofwHistory replays every caches walk's slice of the transcript.
func checkSofwHistory(t *testing.T, a *sofwAdapter, g *tracecheck.Graph) {
	t.Helper()
	entries := sessionHistory(t, a.s.Home, a.id)
	sends := 0
	for _, w := range a.walks {
		steps := sofwHistory(entries[w[0]:w[1]])
		for _, s := range steps {
			if s.Action == "Page#0.TypeSend" {
				sends++
			}
		}
		checkHistory(t, g, entries[w[0]:w[1]], sofwHistory)
		if os.Getenv("SOFW_DEBUG") != "" {
			for _, e := range entries[w[0]:w[1]] {
				tx, _ := e.Data["text"].(string)
				if len(tx) > 60 {
					tx = tx[:60]
				}
				t.Logf("DBG %d %s %q", w[0], e.Kind, tx)
			}
		}
	}
	if sends == 0 {
		t.Fatal("no walk sent \"/a\"; the child went unchecked")
	}
}

func sofwTestdata() string {
	return filepath.Join(filepath.Dir(specPath("sticky_override_file_writers")), "..", "testdata", "sticky_override_file_writers")
}

// TestStickyOverrideFileWritersPaths needs no fizz tools: the walks come
// from the checked-in graph.
func TestStickyOverrideFileWritersPaths(t *testing.T) {
	t.Parallel()
	a := newSofwAdapter(t)
	for _, b := range walkSofwPaths(t, a, envCover()) {
		t.Error(b)
	}
	t.Logf("sticky_override_file_writers: %d caches walks with sends; a mounting child refused /rules %d times", len(a.walks), a.bootRefused)
	g, err := tracecheck.Load(sofwTestdata())
	if err != nil {
		t.Fatal(err)
	}
	checkSofwHistory(t, a, g)
}

// The trace check can fail: a child that refused "/a" and then ran it
// is a path only when a fresh process (its "engine" entry) came between.
func TestStickyOverrideFileWritersHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(sofwTestdata())
	if err != nil {
		t.Fatal(err)
	}
	entry := func(kind, text string) history.Entry {
		return history.Entry{Kind: kind, Data: map[string]any{"text": text}}
	}
	refusedThenRan := func(engine bool) []history.Entry {
		es := []history.Entry{
			entry("command", "/"+sofwA), entry("system", "unknown command: /"+sofwA+" (try /help)"),
			entry("command", "/"+sofwA), entry("system", "/"+sofwA),
			entry("input", "/"+sofwA+"\n\n[skill: "+sofwA+"]\nbody"),
		}
		if engine {
			es = append(es, entry("engine", ""))
		}
		return append(es, entry("assistant", "ok"), entry("done", ""))
	}
	if v := g.Check(sofwHistory(refusedThenRan(true))); v != nil {
		t.Errorf("a refusal, a fresh child, then a run: %v", v)
	}
	if v := g.Check(sofwHistory(refusedThenRan(false))); v == nil {
		t.Error("a child that refused /a ran it with no fresh process between, and the trace check passed")
	} else {
		t.Logf("without the fresh child: %v", v)
	}
}

// The path walk must fail an adapter whose switch posts the wrong id:
// off.yml then never follows the page's click.
func TestStickyOverrideFileWritersPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSofwAdapter(t)
	a.wrongID = true
	if len(walkSofwPaths(t, a, envCover())) == 0 {
		t.Fatal("a path walk whose switch posts a hook's id passed; it is not checking state")
	}
}

func TestStickyOverrideFileWriters(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSofwAdapter(t)
	if err := runMBT(t, "sticky_override_file_writers", a, sofwActions, sofwOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestStickyOverrideFileWritersCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSofwAdapter(t)
	a.wrongID = true
	if err := runMBT(t, "sticky_override_file_writers", a, sofwActions, sofwOptions()); err == nil {
		t.Fatal("a run whose switch posts a hook's id passed; the runner is not checking state")
	}
}
