//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/history_focus_navigation.fizz at the server level: one tab's
// history and focus against a real serve.
//
// The history stack, the focus, the palette and the F6 stop are the
// page's own: nothing on the server sees them, so the adapter keeps
// them by the page's rules, as the spec states them. What the server
// decides it reads, every time a page would:
//
//   - err is S's row: its last turn failed (a real turn, failed through
//     llm-control).
//   - every route a move lands on is read the way its page reads it: S's
//     transcript, the turn's edits behind the "N files" link
//     (edits?turn=N must list the file the turn wrote), S's Context, the
//     project list, and the session list, which must hold S's row.
//   - focus on S's row, or on the ErrorCard's Retry, is only reported
//     while the server gives the page that element: S listed, an error
//     entry in S's transcript. Otherwise nothing holds focus (body).
//
// Where the spec is ahead of the product (Esc's replace, the chip's
// replace, the phone header Back's push, focus after a palette pick or
// a phone pop, the kept F6 stop) is all in the page: this level cannot
// see it, and the browser flow is where it goes red.
type historyFocusAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	work string // S's git checkout, shared by every walk's S
	gate gate

	// S is new per walk: an ErrorCard stays in a transcript for good, and
	// the spec's walk starts with none.
	sid  string
	sids []string
	seq  int64 // the input seq of S's turn that wrote a file: ?turn=N

	phone               bool
	hist                []string
	idx                 int
	focus, stop, opener string
	pal                 bool
	last, mode          string
	seen                []string
	backOn, fwdOn       bool

	walk int // turn names are unique across walks: the queue is shared

	// failAsDone is the deliberate bug
	// TestHistoryFocusNavigationCatchesWrongAdapter injects: the failing
	// turn ErrorCardMount starts is answered as a success.
	failAsDone bool
}

func newHistoryFocusAdapter(t *testing.T) *historyFocusAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &historyFocusAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	a.work = s.Dir(t, "work")
	for _, args := range [][]string{{"init", "-q", "-b", "main"}, {"commit", "-q", "--allow-empty", "-m", "base"}} {
		if err := a.git(args...); err != nil {
			t.Fatal(err)
		}
	}
	return a
}

// git runs git in S's checkout with the serve's HOME, never the user's.
func (a *historyFocusAdapter) git(args ...string) error {
	cmd := exec.Command("git", append([]string{"-C", a.work, "-c", "user.name=model", "-c", "user.email=model@test", "-c", "commit.gpgsign=false"}, args...)...)
	cmd.Env = append(os.Environ(), "HOME="+a.s.Home, "GIT_CONFIG_NOSYSTEM=1")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git %v: %v\n%s", args, err, out)
	}
	return nil
}

// Init is a fresh desktop tab on #/s/S, where S has one finished turn
// that wrote a file (so its "N files" link is there) and no failure.
func (a *historyFocusAdapter) Init() error {
	a.gate.reset()
	a.phone, a.hist, a.idx = false, []string{"s"}, 0
	a.focus, a.stop, a.opener, a.pal = "body", "row", "", false
	a.last, a.mode, a.seen = "load", "", []string{}
	a.arrows()
	a.walk++
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.sid = row.ID
	a.sids = append(a.sids, row.ID)
	name := fmt.Sprintf("h%05d", a.walk)
	content := fmt.Sprintf("walk %d\n", a.walk)
	control.Queue(a.t, a.dir, name+"a", control.Turn{Mode: "ok", Calls: []control.Call{{Name: "write", Args: map[string]any{"path": "f", "content": content}}}})
	control.Queue(a.t, a.dir, name+"b", control.Turn{Mode: "ok", Text: "wrote f"})
	if err := a.s.Prompt(ctx, a.sid, "write f "+name); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.sid, "S's first turn done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	_, lines, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return err
	}
	a.seq = 0
	for _, l := range lines {
		switch {
		case l.Kind == "input":
			a.seq = l.Seq
		case l.Kind == "done" && !slices.Contains(doneFiles(l), "f"):
			return fmt.Errorf("S's turn wrote f but its done lists %v: the page has no \"N files\" link", l.Data["files"])
		}
	}
	return a.show("s")
}

func doneFiles(l serve.Line) []string {
	raw, _ := l.Data["files"].([]any)
	var out []string
	for _, f := range raw {
		if s, ok := f.(string); ok {
			out = append(out, filepath.Base(s))
		}
	}
	return out
}

func (a *historyFocusAdapter) Cleanup() error { return nil }

func (a *historyFocusAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Nav", Index: 0}: a}, nil
}

// GetState is the Nav role: err off S's row, focus on S's row or on
// Retry only while the server gives the page that element, the rest as
// the page holds it.
func (a *historyFocusAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return nil, err
	}
	focus := a.focus
	switch focus {
	case "row":
		listed, err := a.listed()
		if err != nil {
			return nil, err
		}
		if !listed {
			focus = "body"
		}
	case "retry":
		if !hasErrorCard(lines) {
			focus = "body"
		}
	}
	return map[string]any{
		"phone": a.phone, "hist": slices.Clone(a.hist), "idx": a.idx,
		"focus": focus, "stop": a.stop, "err": row.Status == serve.StatusError,
		"pal": a.pal, "opener": a.opener, "last": a.last, "mode": a.mode,
		"seen": slices.Clone(a.seen), "back_on": a.backOn, "fwd_on": a.fwdOn,
	}, nil
}

func hasErrorCard(lines []serve.Line) bool {
	return slices.ContainsFunc(lines, func(l serve.Line) bool { return l.Kind == "error" })
}

// listed is whether the session list holds S's row: the sidebar's
// tree, or the phone's list pane.
func (a *historyFocusAdapter) listed() (bool, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return false, err
	}
	return slices.ContainsFunc(rows, func(r serve.Row) bool { return r.ID == a.sid }), nil
}

// get is a GET the page makes that servetest has no helper for.
func (a *historyFocusAdapter) get(path string, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s: %d", path, resp.StatusCode)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// show is the reads the page makes to put route r up. A read that
// fails is an error: every route this spec visits must render.
func (a *historyFocusAdapter) show(r string) error {
	sess := "/api/sessions/" + url.PathEscape(a.sid)
	switch r {
	case "s":
		ctx, cancel := actionCtx()
		defer cancel()
		_, _, err := a.s.GetSession(ctx, a.sid)
		return err
	case "chg":
		var e struct{ Files []serve.Edit }
		if err := a.get(fmt.Sprintf("%s/edits?turn=%d", sess, a.seq), &e); err != nil {
			return err
		}
		if !slices.ContainsFunc(e.Files, func(f serve.Edit) bool { return filepath.Base(f.Path) == "f" }) {
			return fmt.Errorf("Changes for turn %d lists %v, not the f it wrote", a.seq, e.Files)
		}
		return nil
	case "ctx":
		return a.get(sess+"/context", nil)
	case "proj":
		return a.get("/api/projects", nil)
	case "home":
		listed, err := a.listed()
		if err == nil && !listed {
			err = fmt.Errorf("the session list does not hold S (%s)", a.sid)
		}
		return err
	}
	return fmt.Errorf("no route %q", r)
}

// --- the page's history and focus rules (the spec's helpers) ---

func (a *historyFocusAdapter) cur() string { return a.hist[a.idx] }

func (a *historyFocusAdapter) prev() string {
	if a.idx == 0 {
		return ""
	}
	return a.hist[a.idx-1]
}

func hfLand(r string, phone bool) string {
	if phone && r == "home" {
		return "row"
	}
	return "app"
}

func hfRegions(r string) []string {
	switch r {
	case "s":
		return []string{"side", "head", "transcript", "composer"}
	case "home":
		return []string{"side"}
	}
	return []string{"side", "head"}
}

func hfRegionOf(f string) string {
	switch f {
	case "row", "other", "nav":
		return "side"
	case "retry":
		return "transcript"
	case "head", "transcript", "composer":
		return f
	}
	return ""
}

func (a *historyFocusAdapter) explore() bool {
	return a.idx == len(a.hist)-1 && len(a.hist) <= 2 && !a.errNow()
}

// errNow is err as the page knows it: S's row.
func (a *historyFocusAdapter) errNow() bool {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.sid)
	return err == nil && row.Status == serve.StatusError
}

func (a *historyFocusAdapter) arrows() {
	a.backOn = !a.phone && a.idx > 0
	a.fwdOn = !a.phone && a.idx+1 < len(a.hist)
}

func (a *historyFocusAdapter) push(r string) error {
	a.hist = append(slices.Clone(a.hist[:a.idx+1]), r)
	a.idx++
	a.last, a.stop = "push", "row"
	a.arrows()
	return a.show(r)
}

func (a *historyFocusAdapter) settle(r string) error {
	if a.prev() == r {
		a.idx--
		a.last = "back"
	} else {
		h := slices.Clone(a.hist)
		h[a.idx] = r
		a.hist = h
		a.last = "replace"
	}
	a.stop = "row"
	a.arrows()
	return a.show(r)
}

func (a *historyFocusAdapter) back() error {
	a.idx--
	a.last, a.stop = "back", "row"
	a.arrows()
	return a.show(a.cur())
}

func (a *historyFocusAdapter) forward() error {
	a.idx++
	a.last, a.stop = "fwd", "row"
	a.arrows()
	return a.show(a.cur())
}

func (a *historyFocusAdapter) landAfterPop(was string) {
	if was == "composer" && a.cur() == "s" {
		a.focus = "composer"
	} else {
		a.focus = hfLand(a.cur(), a.phone)
	}
}

func (a *historyFocusAdapter) headerBack() error {
	var err error
	if a.prev() == "home" {
		err = a.back()
	} else {
		err = a.push("home")
	}
	a.focus = hfLand("home", true)
	return err
}

func (a *historyFocusAdapter) closeSub() error {
	err := a.settle("s")
	a.focus = hfLand("s", a.phone)
	return err
}

func (a *historyFocusAdapter) f6(step int) string {
	rs := hfRegions(a.cur())
	g := hfRegionOf(a.focus)
	i := slices.Index(rs, g)
	var n string
	if i < 0 {
		n = rs[0]
	} else {
		n = rs[(i+step+len(rs))%len(rs)]
	}
	if n == "side" {
		a.focus = a.stop
	} else {
		a.focus = n
	}
	return n
}

// --- actions ---

func (a *historyFocusAdapter) UsePhone() error {
	if !a.gate.pass(a.mode == "" && !a.phone && a.idx == 0 && slices.Equal(a.hist, []string{"s"}) && a.focus == "body" && !a.errNow() && a.last == "load") {
		return nil
	}
	a.phone, a.hist = true, []string{"home"}
	a.arrows()
	return a.show("home")
}

func (a *historyFocusAdapter) ClickRow() error {
	if !a.gate.pass(a.mode == "" && !a.pal && a.cur() != "s" && a.idx < 2 && (!a.phone || a.cur() == "home")) {
		return nil
	}
	err := a.push("s")
	if a.phone {
		a.focus = hfLand("s", true)
	} else {
		a.focus = "row"
	}
	return err
}

func (a *historyFocusAdapter) OpenChangesLink() error {
	if !a.gate.pass(a.mode == "" && !a.pal && a.cur() == "s" && a.idx < 2) {
		return nil
	}
	err := a.push("chg")
	a.focus = hfLand("chg", a.phone)
	return err
}

func (a *historyFocusAdapter) NavProjects() error {
	if !a.gate.pass(a.mode == "" && !a.pal && a.cur() != "proj" && a.idx < 2 && (!a.phone || a.cur() == "home")) {
		return nil
	}
	err := a.push("proj")
	a.focus = "nav"
	return err
}

func (a *historyFocusAdapter) PhoneHeaderBack() error {
	if !a.gate.pass(a.mode == "" && a.phone && a.cur() == "s" && (a.idx < 2 || a.prev() == "home")) {
		return nil
	}
	return a.headerBack()
}

func (a *historyFocusAdapter) OpenContext() error {
	if !a.gate.pass(a.mode == "" && !a.pal && !a.phone && a.cur() == "s" && a.idx < 2) {
		return nil
	}
	err := a.push("ctx")
	a.focus = hfLand("ctx", false)
	return err
}

func (a *historyFocusAdapter) CloseSub() error {
	if !a.gate.pass(a.mode == "" && !a.pal && (a.cur() == "chg" || a.cur() == "ctx")) {
		return nil
	}
	return a.closeSub()
}

func (a *historyFocusAdapter) BrowserBack() error {
	if !a.gate.pass(a.mode == "" && !a.pal && a.idx > 0) {
		return nil
	}
	was := a.focus
	err := a.back()
	a.landAfterPop(was)
	return err
}

func (a *historyFocusAdapter) BrowserForward() error {
	if !a.gate.pass(a.mode == "" && !a.pal && a.idx+1 < len(a.hist)) {
		return nil
	}
	was := a.focus
	err := a.forward()
	a.landAfterPop(was)
	return err
}

// Reload reads the entry again; the ErrorCard mounting onto body takes
// focus when S's transcript holds one.
func (a *historyFocusAdapter) Reload() error {
	if !a.gate.pass(len(a.hist) <= 2 && a.mode == "" && !a.pal && (a.phone || a.cur() != "home")) {
		return nil
	}
	a.last, a.stop, a.focus = "load", "row", "body"
	if err := a.show(a.cur()); err != nil {
		return err
	}
	if a.cur() == "s" {
		ctx, cancel := actionCtx()
		defer cancel()
		_, lines, err := a.s.GetSession(ctx, a.sid)
		if err != nil {
			return err
		}
		if hasErrorCard(lines) {
			a.focus = "retry"
		}
	}
	return nil
}

func (a *historyFocusAdapter) OpenPalette() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone) {
		a.pal, a.opener, a.focus = true, a.focus, "palfield"
	}
	return nil
}

func (a *historyFocusAdapter) PalClose() error {
	if a.gate.pass(a.pal) {
		a.pal, a.focus, a.opener = false, a.opener, ""
	}
	return nil
}

func (a *historyFocusAdapter) PalPickSession() error {
	if !a.gate.pass(a.pal && a.cur() != "s" && a.idx < 2) {
		return nil
	}
	a.pal, a.opener = false, ""
	err := a.push("s")
	a.focus = hfLand("s", false)
	return err
}

func (a *historyFocusAdapter) PalGoSessions() error {
	if !a.gate.pass(a.pal && a.cur() != "home" && a.idx < 2) {
		return nil
	}
	a.pal, a.opener = false, ""
	err := a.push("home")
	a.focus = hfLand("home", false)
	return err
}

func (a *historyFocusAdapter) F6() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone) {
		a.f6(1)
	}
	return nil
}

func (a *historyFocusAdapter) ShiftF6() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone) {
		a.f6(-1)
	}
	return nil
}

func (a *historyFocusAdapter) AltI() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone && a.cur() == "s" && a.focus != "composer") {
		a.focus = "composer"
	}
	return nil
}

func (a *historyFocusAdapter) ArrowInTree() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone && a.focus == "row") {
		a.focus, a.stop = "other", "other"
	}
	return nil
}

// ErrorCardMount is a real failing turn of S's; the card takes focus
// only when nothing holds it, and only once the transcript has it.
func (a *historyFocusAdapter) ErrorCardMount() error {
	if !a.gate.pass(!a.phone && len(a.hist) == 1 && a.mode == "" && !a.pal && !a.errNow() && a.cur() == "s") {
		return nil
	}
	name := fmt.Sprintf("h%05dc", a.walk)
	turn := control.Turn{Mode: "error", Error: "model says no"}
	if a.failAsDone {
		turn = control.Turn{Mode: "ok", Text: "fine"}
	}
	control.Queue(a.t, a.dir, name, turn)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.sid, "fail "+name); err != nil {
		return err
	}
	row, err := waitRow(a.s, a.sid, "S's second turn to end", func(r serve.Row) bool {
		return r.Status == serve.StatusError || r.Status == serve.StatusDone
	})
	if err != nil {
		return err
	}
	if row.Status == serve.StatusDone {
		// A done row may be the first turn's still; wait for the second.
		if _, err := a.waitDones(2); err != nil {
			return err
		}
	}
	_, lines, err := a.s.GetSession(ctx, a.sid)
	if err != nil {
		return err
	}
	if a.focus == "body" && hasErrorCard(lines) {
		a.focus = "retry"
	}
	return nil
}

// waitDones waits for S's nth closed turn.
func (a *historyFocusAdapter) waitDones(n int) (serve.Row, error) {
	return waitRow(a.s, a.sid, fmt.Sprintf("S's turn %d to close", n), func(r serve.Row) bool {
		ctx, cancel := actionCtx()
		defer cancel()
		_, lines, err := a.s.GetSession(ctx, a.sid)
		if err != nil || r.Status == serve.StatusRunning {
			return false
		}
		closed := 0
		for _, l := range lines {
			if l.Kind == "done" || l.Kind == "cancelled" {
				closed++
			}
		}
		return closed >= n
	})
}

func (a *historyFocusAdapter) StartLeaving() error {
	if a.gate.pass(a.mode == "" && !a.pal && a.idx > 0) {
		a.mode = "leaving"
	}
	return nil
}

func (a *historyFocusAdapter) Leave() error {
	if !a.gate.pass(a.mode == "leaving") {
		return nil
	}
	was := a.focus
	var err error
	switch {
	case a.phone && a.cur() == "s" && (a.idx < 2 || a.prev() == "home"):
		err = a.headerBack()
	case a.cur() == "chg" || a.cur() == "ctx":
		err = a.closeSub()
	case a.idx > 0:
		err = a.back()
		a.landAfterPop(was)
	}
	if a.idx == 0 {
		a.mode = ""
	}
	return err
}

func (a *historyFocusAdapter) StartCycling() error {
	if a.gate.pass(a.explore() && a.mode == "" && !a.pal && !a.phone) {
		a.mode, a.seen = "cycling", []string{}
	}
	return nil
}

func (a *historyFocusAdapter) Cycle() error {
	if !a.gate.pass(a.mode == "cycling") {
		return nil
	}
	g := a.f6(1)
	var seen []string
	for _, x := range []string{"side", "head", "transcript", "composer"} {
		if slices.Contains(a.seen, x) || x == g {
			seen = append(seen, x)
		}
	}
	a.seen = seen
	covered := true
	for _, r := range hfRegions(a.cur()) {
		if !slices.Contains(a.seen, r) {
			covered = false
		}
	}
	if covered {
		a.mode, a.seen = "", []string{}
	}
	return nil
}

var historyFocusNavigationActions = map[string]map[string]fmbt.ActionFunc{"Nav": {
	"UsePhone":        action((*historyFocusAdapter).UsePhone),
	"ClickRow":        action((*historyFocusAdapter).ClickRow),
	"OpenChangesLink": action((*historyFocusAdapter).OpenChangesLink),
	"NavProjects":     action((*historyFocusAdapter).NavProjects),
	"PhoneHeaderBack": action((*historyFocusAdapter).PhoneHeaderBack),
	"OpenContext":     action((*historyFocusAdapter).OpenContext),
	"CloseSub":        action((*historyFocusAdapter).CloseSub),
	"BrowserBack":     action((*historyFocusAdapter).BrowserBack),
	"BrowserForward":  action((*historyFocusAdapter).BrowserForward),
	"Reload":          action((*historyFocusAdapter).Reload),
	"OpenPalette":     action((*historyFocusAdapter).OpenPalette),
	"PalClose":        action((*historyFocusAdapter).PalClose),
	"PalPickSession":  action((*historyFocusAdapter).PalPickSession),
	"PalGoSessions":   action((*historyFocusAdapter).PalGoSessions),
	"F6":              action((*historyFocusAdapter).F6),
	"ShiftF6":         action((*historyFocusAdapter).ShiftF6),
	"AltI":            action((*historyFocusAdapter).AltI),
	"ArrowInTree":     action((*historyFocusAdapter).ArrowInTree),
	"ErrorCardMount":  action((*historyFocusAdapter).ErrorCardMount),
	"StartLeaving":    action((*historyFocusAdapter).StartLeaving),
	"Leave":           action((*historyFocusAdapter).Leave),
	"StartCycling":    action((*historyFocusAdapter).StartCycling),
	"Cycle":           action((*historyFocusAdapter).Cycle),
}}

// Every Init is a real turn, and most random walks end early on a
// disabled action among 23, so many short walks. Seedless.
func historyFocusNavigationOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// historyFocusNavigationHistory: navigation and focus leave nothing in
// a transcript. What one S's history says is whether its tab saw the
// ErrorCard mount: a closed turn after the first that ended in an error.
func historyFocusNavigationHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Nav#0.err": false}}}
	inputs, failed := 0, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			inputs++
		case "error":
			failed = failed || inputs >= 2
		}
	}
	if failed {
		steps = append(steps, tracecheck.Step{Action: "Nav#0.ErrorCardMount", State: map[string]any{"Nav#0.err": true}})
	}
	return steps
}

func init() { historyProjections["history_focus_navigation"] = historyFocusNavigationHistory }

func TestHistoryFocusNavigation(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryFocusAdapter(t)
	if err := runMBT(t, "history_focus_navigation", a, historyFocusNavigationActions, historyFocusNavigationOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	a.checkHistories(t)
}

func (a *historyFocusAdapter) checkHistories(t *testing.T) {
	g, err := tracecheck.Load(fizzCheck(t, "history_focus_navigation"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.sids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), historyFocusNavigationHistory)
	}
}

// TestHistoryFocusNavigationPaths walks the graph's paths through the
// adapter step by step against the server: every settled state by
// default, every transition under MODEL_COVER=transitions.
func TestHistoryFocusNavigationPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	raw, err := pathsJSON("history_focus_navigation")
	if err != nil {
		t.Fatal(err)
	}
	a := newHistoryFocusAdapter(t)
	if err := walkHistoryFocusPaths(a, raw); err != nil {
		t.Error(err)
	}
	a.checkHistories(t)
}

// walkHistoryFocusPaths runs each path's actions and compares the
// role's state with the path's after every step, stopping at the first
// path that disagrees.
func walkHistoryFocusPaths(a *historyFocusAdapter, raw []byte) error {
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	for pi, p := range doc.Paths {
		var names []string
		for _, st := range p.Trace {
			names = append(names, strings.TrimPrefix(st.Action, "Nav#0."))
		}
		for i, step := range p.Trace {
			var err error
			if i == 0 {
				err = a.Init()
			} else if f, ok := historyFocusNavigationActions["Nav"][names[i]]; !ok {
				err = fmt.Errorf("no such action")
			} else {
				_, err = f(a, nil)
			}
			if err == nil && a.gate.off {
				err = fmt.Errorf("the adapter's require says disabled")
			}
			var got map[string]any
			if err == nil {
				got, err = a.GetState()
			}
			if err == nil {
				want := map[string]any{}
				for k, v := range step.State {
					if f, ok := strings.CutPrefix(k, "Nav#0."); ok {
						want[f] = v
					}
				}
				if !reflect.DeepEqual(normalize(got), want) {
					err = fmt.Errorf("state\n got %v\nwant %v", got, want)
				}
			}
			if err != nil {
				return fmt.Errorf("path %d %v, step %d (%s): %w", pi, names, i, names[i], err)
			}
		}
	}
	return nil
}

// normalize puts GetState's values in the shapes a decoded trace has:
// JSON numbers and lists.
func normalize(m map[string]any) map[string]any {
	b, _ := json.Marshal(m)
	var out map[string]any
	json.Unmarshal(b, &out)
	return out
}

// The run proves nothing unless a server that breaks the model fails
// it. This adapter answers the turn ErrorCardMount means to fail as a
// success: S's row says done, not error, and the walk must say so.
func TestHistoryFocusNavigationCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	raw, err := pathsJSON("history_focus_navigation")
	if err != nil {
		t.Fatal(err)
	}
	a := newHistoryFocusAdapter(t)
	a.failAsDone = true
	err = walkHistoryFocusPaths(a, raw)
	if err == nil {
		t.Fatal("paths whose failing turn ends as done passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
