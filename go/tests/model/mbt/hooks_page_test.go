//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
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

// specs/hooks_page.fizz against a real serve: the adapter is the Hooks
// page's client. What the page holds itself (list, file, buffer, note)
// is tracked here from the answers serve gives; everything serve owns
// (the file on disk, the off switch, the ledger, what refused callers
// managed to write) is read back from serve or its HOME every step.

const (
	// The hook the page opens. pre-code-exec never fires in these walks
	// (llm-control replies with text, no code runs), so its edits and
	// its switch cannot change what a Fire turn does.
	hooksPageEvent = "pre-code-exec"
	hooksPageName  = "guard.js"
	// The hook a Fire turn trips: always on, never edited.
	hooksPageFireEvent = "user-prompt-submit"
	hooksPageFireName  = "fire.js"

	hooksPageOK     = "// guard: returns\nreturn {};\n"
	hooksPageBroken = "// guard: throws\nthrow new Error(\"guard is broken\");\n"
)

type hooksPageAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	hook       string // guard.js's absolute path
	outside    []string
	rule       string // a rule file under ~/.claude/rules the page lists read-only
	ruleBody   string
	offYML     string
	turn       int
	fireIDs    []string // one session per Fire; each is trace-checked
	seenNewest string   // the newest fire the page has read, "" for none

	list, file, buffer, note string

	// toggleWrongKind is the deliberate bug TestHooksPageCatchesWrongAdapter
	// injects: Toggle throws the switch under "watcher:", not "hook:".
	toggleWrongKind bool
}

func newHooksPageAdapter(t *testing.T) *hooksPageAdapter {
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files: map[string]string{
			".bough/hooks/" + hooksPageEvent + "/" + hooksPageName:         hooksPageOK,
			".bough/hooks/" + hooksPageFireEvent + "/" + hooksPageFireName: "// fire: records a fire\nreturn {};\n",
			".claude/rules/style.md":                                       "Write tests first.\n",
		},
	})
	a := &hooksPageAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	a.hook = filepath.Join(s.Home, ".bough", "hooks", hooksPageEvent, hooksPageName)
	a.rule = filepath.Join(s.Home, ".claude", "rules", "style.md")
	a.ruleBody = "Write tests first.\n"
	a.offYML = filepath.Join(s.Home, ".bough", "off.yml")
	// A symlink in the pool pointing out of it: writing through it
	// would land outside the pools.
	elsewhere := s.Dir(t, "elsewhere")
	if err := os.Symlink(elsewhere, filepath.Join(s.Home, ".bough", "hooks", hooksPageEvent, "out")); err != nil {
		t.Fatal(err)
	}
	// Each file a refused PUT would have created.
	a.outside = []string{
		filepath.Join(s.Home, "outside.js"),
		filepath.Join(elsewhere, "evil.js"),
		filepath.Join(s.Home, ".bough", "hooks", hooksPageEvent, "notes.md"),
	}
	return a
}

// Init is a fresh page on the same serve: the hook back to "ok" and on,
// and every fire already in the ledger counted as seen, because a new
// walk is a new page and the spec starts it with nothing unshown.
func (a *hooksPageAdapter) Init() error {
	if err := os.WriteFile(a.hook, []byte(hooksPageOK), 0o644); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.call(ctx, a.s.Token, http.MethodPost, "/api/off", map[string]any{"id": "hook:" + hooksPageEvent + "/" + hooksPageName, "off": false}, nil); err != nil {
		return err
	}
	d, err := a.read(ctx)
	if err != nil {
		return err
	}
	a.seenNewest = newestFire(d)
	a.list, a.file, a.buffer, a.note = "loading", "closed", "ok", ""
	a.gate.reset()
	return nil
}

func (a *hooksPageAdapter) Cleanup() error { return nil }

func (a *hooksPageAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// GetState reads what serve owns fresh every step: the file's bytes,
// the switch as /api/hooks reports it, the ledger's newest fire, and
// whether any refused write left a trace on disk.
func (a *hooksPageAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	d, err := a.read(ctx)
	if err != nil {
		return nil, err
	}
	disk, err := a.disk()
	if err != nil {
		return nil, err
	}
	row, err := guardRow(d)
	if err != nil {
		return nil, err
	}
	outside := false
	for _, p := range a.outside {
		if _, err := os.Lstat(p); err == nil {
			outside = true
		}
	}
	if b, err := os.ReadFile(a.rule); err != nil || string(b) != a.ruleBody {
		outside = true
	}
	off, _ := os.ReadFile(a.offYML)
	return map[string]any{
		"list":           a.list,
		"unshown":        newestFire(d) != a.seenNewest,
		"file":           a.file,
		"disk":           disk,
		"buffer":         a.buffer,
		"note":           a.note,
		"hook_off":       row.Off,
		"wrote_outside":  outside,
		"wrote_bad_kind": bytes.Contains(off, []byte("bogus")),
	}, nil
}

// --- the list ---------------------------------------------------------

func (a *hooksPageAdapter) Poll() error {
	if !a.gate.pass(a.file == "closed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	d, err := a.read(ctx)
	if err != nil {
		return err
	}
	a.list, a.seenNewest = "loaded", newestFire(d)
	return nil
}

// PollFail is a read that fails. The page cannot tell one failure from
// another, so the adapter makes the real request with a token serve
// does not know and requires serve to refuse it.
func (a *hooksPageAdapter) PollFail() error {
	if !a.gate.pass(a.file == "closed") {
		return nil
	}
	if err := a.failedRead("/api/hooks"); err != nil {
		return err
	}
	if a.list == "loading" || a.list == "error" {
		a.list = "error"
	} else {
		a.list = "stale"
	}
	return nil
}

// Fire runs one turn in a new session whose user-prompt-submit hook
// fires, then waits until the ledger /api/hooks serves has it: the
// spec's claim is that the next poll shows it.
func (a *hooksPageAdapter) Fire() error {
	ctx, cancel := actionCtx()
	defer cancel()
	d, err := a.read(ctx)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.file == "closed" && newestFire(d) == a.seenNewest) {
		return nil
	}
	before := newestFire(d)
	a.turn++
	name := fmt.Sprintf("h%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "fired " + name})
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "turn "+name)
	if err != nil {
		return err
	}
	a.fireIDs = append(a.fireIDs, row.ID)
	if _, err := waitRow(a.s, row.ID, "the fire turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	for {
		d, err := a.read(ctx)
		if err != nil {
			return err
		}
		if newestFire(d) != before {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("the fire of session %s never reached /api/hooks", row.ID)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// --- the file ---------------------------------------------------------

func (a *hooksPageAdapter) Open() error {
	ctx, cancel := actionCtx()
	defer cancel()
	d, err := a.read(ctx)
	if err != nil {
		return err
	}
	row, err := guardRow(d)
	if err != nil {
		return err
	}
	if !a.gate.pass(a.list == "loaded" && !row.Off && newestFire(d) == a.seenNewest && a.file == "closed") {
		return nil
	}
	a.file = "loading"
	return nil
}

func (a *hooksPageAdapter) Loaded() error {
	if !a.gate.pass(a.file == "loading") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var r struct {
		Body string `json:"body"`
	}
	if _, err := a.call(ctx, a.s.Token, http.MethodGet, "/api/hooks/file?path="+url.QueryEscape(a.hook), nil, &r); err != nil {
		return err
	}
	a.file, a.buffer = "open", version(r.Body)
	return nil
}

// LoadFail is the file read failing; as with PollFail, a refused
// request stands for any failure, and a late answer the page's timer
// dropped looks the same to it.
func (a *hooksPageAdapter) LoadFail() error {
	if !a.gate.pass(a.file == "loading") {
		return nil
	}
	if err := a.failedRead("/api/hooks/file?path=" + url.QueryEscape(a.hook)); err != nil {
		return err
	}
	a.file = "error"
	return nil
}

func (a *hooksPageAdapter) FileRetry() error {
	if a.gate.pass(a.file == "error") {
		a.file = "loading"
	}
	return nil
}

func (a *hooksPageAdapter) Edit() error {
	if !a.gate.pass(a.file == "open") {
		return nil
	}
	if a.buffer == "ok" {
		a.buffer = "broken"
	} else {
		a.buffer = "ok"
	}
	a.note = ""
	return nil
}

func (a *hooksPageAdapter) Save() error {
	disk, err := a.disk()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.file == "open" && a.buffer != disk) {
		return nil
	}
	body := hooksPageOK
	if a.buffer == "broken" {
		body = hooksPageBroken
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.call(ctx, a.s.Token, http.MethodPut, "/api/hooks/file", map[string]any{"path": a.hook, "body": body}, nil); err != nil {
		return err
	}
	a.note = "saved"
	return nil
}

// DryRun sends what Source sends: the path and the row's event.
func (a *hooksPageAdapter) DryRun() error {
	if !a.gate.pass(a.file == "open" && a.note != "ran" && a.note != "failed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	var r struct {
		Error string `json:"error"`
	}
	if _, err := a.call(ctx, a.s.Token, http.MethodPost, "/api/hooks/dryrun", map[string]any{"path": a.hook, "event": hooksPageEvent}, &r); err != nil {
		return err
	}
	if r.Error == "" {
		a.note = "ran"
	} else {
		a.note = "failed"
	}
	return nil
}

// --- the off switch ---------------------------------------------------

func (a *hooksPageAdapter) Toggle() error {
	if !a.gate.pass((a.list == "loaded" || a.list == "stale") && a.file == "closed") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	d, err := a.read(ctx)
	if err != nil {
		return err
	}
	row, err := guardRow(d)
	if err != nil {
		return err
	}
	kind := "hook"
	if a.toggleWrongKind {
		kind = "watcher"
	}
	_, err = a.call(ctx, a.s.Token, http.MethodPost, "/api/off", map[string]any{"id": kind + ":" + row.ID, "off": !row.Off}, nil)
	return err
}

// --- callers serve must refuse ------------------------------------------

// PutOutside tries the three ways out of the pools and one rule file.
// Whether each was refused is read back from disk in GetState; a 400
// is what the page's client would see, so anything else is a failure.
func (a *hooksPageAdapter) PutOutside() error {
	if !a.gate.pass(a.list == "loading") {
		return nil
	}
	pool := filepath.Join(a.s.Home, ".bough", "hooks", hooksPageEvent)
	tries := []string{
		pool + "/../../../outside.js",
		filepath.Join(pool, "out", "evil.js"),
		filepath.Join(pool, "notes.md"),
		a.rule,
	}
	ctx, cancel := actionCtx()
	defer cancel()
	for _, p := range tries {
		code, _ := a.call(ctx, a.s.Token, http.MethodPut, "/api/hooks/file", map[string]any{"path": p, "body": "written from outside\n"}, nil)
		if code != http.StatusBadRequest {
			return fmt.Errorf("PUT %s: status %d, want 400", p, code)
		}
	}
	return nil
}

func (a *hooksPageAdapter) OffBadKind() error {
	if !a.gate.pass(a.list == "loading") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code, _ := a.call(ctx, a.s.Token, http.MethodPost, "/api/off", map[string]any{"id": "bogus:x", "off": true}, nil)
	if code != http.StatusBadRequest {
		return fmt.Errorf("POST /api/off bogus:x: status %d, want 400", code)
	}
	return nil
}

// --- plumbing -----------------------------------------------------------

type hooksPageData struct {
	Hooks []serve.HookRow  `json:"hooks"`
	Fires []serve.HookFire `json:"fires"`
}

func (a *hooksPageAdapter) read(ctx context.Context) (hooksPageData, error) {
	var d hooksPageData
	_, err := a.call(ctx, a.s.Token, http.MethodGet, "/api/hooks", nil, &d)
	return d, err
}

// failedRead makes a read with a token serve never issued and returns
// an error unless serve refused it.
func (a *hooksPageAdapter) failedRead(path string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	code, err := a.call(ctx, "not-the-token", http.MethodGet, path, nil, nil)
	if err == nil {
		return fmt.Errorf("GET %s with a wrong token: status %d, want a refusal", path, code)
	}
	return nil
}

func (a *hooksPageAdapter) disk() (string, error) {
	b, err := os.ReadFile(a.hook)
	if err != nil {
		return "", err
	}
	return version(string(b)), nil
}

// call is one request to serve's API as the page makes it. servetest
// has no generic call; its URL and Token are all a client needs.
func (a *hooksPageAdapter) call(ctx context.Context, token, method, path string, body, out any) (int, error) {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return resp.StatusCode, err
	}
	if resp.StatusCode/100 != 2 {
		return resp.StatusCode, fmt.Errorf("%s %s: %d: %s", method, path, resp.StatusCode, strings.TrimSpace(string(raw)))
	}
	if out != nil {
		if err := json.Unmarshal(raw, out); err != nil {
			return resp.StatusCode, fmt.Errorf("%s %s: decode: %w", method, path, err)
		}
	}
	return resp.StatusCode, nil
}

// version names which of the two bodies a file holds.
func version(body string) string {
	if strings.Contains(body, "throw") {
		return "broken"
	}
	return "ok"
}

func guardRow(d hooksPageData) (serve.HookRow, error) {
	for _, h := range d.Hooks {
		if h.Event == hooksPageEvent && h.Name == hooksPageName {
			return h, nil
		}
	}
	return serve.HookRow{}, fmt.Errorf("/api/hooks does not list %s/%s", hooksPageEvent, hooksPageName)
}

// newestFire keys the ledger's newest fire. A count would not do: the
// ledger is capped, and a hundred walks fill it.
func newestFire(d hooksPageData) string {
	if len(d.Fires) == 0 {
		return ""
	}
	f := d.Fires[0]
	return f.Session + "@" + f.At.Format("2006-01-02T15:04:05.000000000Z07:00") + "/" + f.Name
}

var hooksPageActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"Poll":       action((*hooksPageAdapter).Poll),
	"PollFail":   action((*hooksPageAdapter).PollFail),
	"Fire":       action((*hooksPageAdapter).Fire),
	"Open":       action((*hooksPageAdapter).Open),
	"Loaded":     action((*hooksPageAdapter).Loaded),
	"LoadFail":   action((*hooksPageAdapter).LoadFail),
	"FileRetry":  action((*hooksPageAdapter).FileRetry),
	"Edit":       action((*hooksPageAdapter).Edit),
	"Save":       action((*hooksPageAdapter).Save),
	"DryRun":     action((*hooksPageAdapter).DryRun),
	"Toggle":     action((*hooksPageAdapter).Toggle),
	"PutOutside": action((*hooksPageAdapter).PutOutside),
	"OffBadKind": action((*hooksPageAdapter).OffBadKind),
}}

// The spec is wider than the example (13 actions, 24 states), and most
// steps are a request, not a turn, so walks are longer and more of them.
func hooksPageOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 10, "max-parallel-runs": 0}
}

// hooksPageHistory reads a Fire session's transcript: the only page
// action history records is a hook firing. A turn's fires reach the
// page together at the next poll, so all of one turn's hook entries
// are one Fire; the adapter gives each Fire its own session, so a
// transcript is Init then at most one Fire.
func hooksPageHistory(entries []history.Entry) []tracecheck.Step {
	unshown := func(v bool) map[string]any { return map[string]any{"Page#0.unshown": v} }
	steps := []tracecheck.Step{{Action: "Init", State: unshown(false)}}
	fired := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			fired = false
		case "hook":
			if !fired {
				fired = true
				steps = append(steps, tracecheck.Step{Action: "Page#0.Fire", State: unshown(true)})
			}
		}
	}
	return steps
}

func init() { historyProjections["hooks_page"] = hooksPageHistory }

func TestHooksPage(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHooksPageAdapter(t)
	if err := runMBT(t, "hooks_page", a, hooksPageActions, hooksPageOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	if len(a.fireIDs) == 0 {
		t.Fatal("no walk fired a hook; the ledger went unchecked")
	}
	g, err := tracecheck.Load(fizzCheck(t, "hooks_page"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.fireIDs {
		entries := sessionHistory(t, a.s.Home, id)
		if steps := hooksPageHistory(entries); len(steps) != 2 {
			t.Errorf("session %s: %d Fire steps in its history, want 1", id, len(steps)-1)
		}
		checkHistory(t, g, entries, hooksPageHistory)
	}
}

// A run where Toggle throws the wrong switch must fail: the row's word
// then never follows the page's click.
func TestHooksPageCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHooksPageAdapter(t)
	a.toggleWrongKind = true
	if err := runMBT(t, "hooks_page", a, hooksPageActions, hooksPageOptions()); err == nil {
		t.Fatal("a run whose Toggle throws a watcher switch passed; the runner is not checking state")
	}
}

// The runner picks each step uniformly from all 13 actions and stops
// checking a walk at the first disabled one, so it almost never gets
// past Open: a 150-walk run executed Loaded, Edit, Save and DryRun zero
// times. So the spec is also walked along the generator's paths
// (testdata/hooks_page/paths.json: every transition at least once, the
// paths the browser spec walks), comparing the adapter's state with the
// spec's after every step. TestSpecFixtures keeps that graph current.
func walkHooksPagePaths(t *testing.T, a *hooksPageAdapter) []string {
	t.Helper()
	b, err := pathsJSON("hooks_page")
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
		t.Fatal("paths.json has no paths")
	}
	var bad []string
	for i, p := range out.Paths {
	steps:
		for j, step := range p.Trace {
			if j == 0 {
				err = a.Init()
			} else {
				f, ok := hooksPageActions["Page"][strings.TrimPrefix(step.Action, "Page#0.")]
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
				if ok && got[field] != want {
					bad = append(bad, fmt.Sprintf("path %d step %d (%s): %s is %v, the spec says %v", i, j, step.Action, field, got[field], want))
					break steps
				}
			}
		}
	}
	return bad
}

func hooksPageTestdata() string {
	return filepath.Join(filepath.Dir(specPath("hooks_page")), "..", "testdata", "hooks_page")
}

// TestHooksPagePaths needs no fizz tools: the paths and the graph are
// the checked-in fixtures.
func TestHooksPagePaths(t *testing.T) {
	t.Parallel()
	a := newHooksPageAdapter(t)
	for _, b := range walkHooksPagePaths(t, a) {
		t.Error(b)
	}
	if len(a.fireIDs) == 0 {
		t.Fatal("no path fired a hook; the ledger went unchecked")
	}
	g, err := tracecheck.Load(hooksPageTestdata())
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.fireIDs {
		entries := sessionHistory(t, a.s.Home, id)
		if steps := hooksPageHistory(entries); len(steps) != 2 {
			t.Errorf("session %s: %d Fire steps in its history, want 1", id, len(steps)-1)
		}
		checkHistory(t, g, entries, hooksPageHistory)
	}
}

// The path walk, like the runner, must fail an adapter wired wrong.
func TestHooksPagePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newHooksPageAdapter(t)
	a.toggleWrongKind = true
	if len(walkHooksPagePaths(t, a)) == 0 {
		t.Fatal("a path walk whose Toggle throws a watcher switch passed; it is not checking state")
	}
}
