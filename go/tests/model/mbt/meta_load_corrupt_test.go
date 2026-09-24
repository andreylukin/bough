//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/meta_load_corrupt.fizz against a real `bough serve --run` that
// the adapter starts, stops and kills on one HOME: the Serve role is the
// process plus the two files it reads before it listens, meta.json and
// serve.token. Every file field is read off the disk after each step, so
// a start that rewrites, migrates or drops a file shows; serve is "up"
// when /api/health answers and "refused" when the last start exited
// before it did (what launchd would restart into).
//
// The faults are made on the files directly, as the world makes them: a
// power loss is SIGKILL to the process group plus the file a
// non-durable save leaves (a zero-length meta.json, or a stray .tmp);
// ENOSPC on the first token write is RLIMIT_FSIZE=0 on that one start,
// so the O_EXCL create succeeds and the write fails (Go ignores
// SIGXFSZ, the write returns EFBIG).

type mlcAdapter struct {
	t      *testing.T
	s      *servetest.Server
	bin    string // the bough binary
	enospc string // the same binary under `ulimit -f 0`
	meta   string // ~/.bough/serve/meta.json
	token  string // ~/.bough/serve.token
	gate   gate

	sid   string // the one session a Save renames
	title string // the title the last Save set; "" before any
	n     int

	up, refused bool
	// good is the sessions table of the last readable file a corrupting
	// edit started from: what a human repairing the file puts back.
	good map[string]any

	// journal is every walk's executed steps, with the files read after
	// each and, for a start, what that process wrote to its log: the
	// trace check replays it with serve read off the log alone.
	journal [][]mlcStep
	taken   map[string]int

	// enospcSucceeds is TestMetaLoadCorruptCatchesWrongAdapter's bug:
	// StartTokenENOSPC starts serve with no size limit, so the token
	// write it is about never fails.
	enospcSucceeds bool
}

// mlcStep is one journaled step: the action, the files after it, and
// for a start the serve output of that process.
type mlcStep struct {
	Action string
	Files  map[string]any
	Start  bool
	Log    string
}

func newMLCAdapter(t *testing.T) *mlcAdapter {
	s := servetest.Start(t, servetest.Options{})
	a := &mlcAdapter{
		t: t, s: s, bin: s.Bin(),
		meta:  filepath.Join(s.Home, ".bough", "serve", "meta.json"),
		token: filepath.Join(s.Home, ".bough", "serve.token"),
		taken: map[string]int{},
	}
	a.enospc = filepath.Join(s.Root, "bough-enospc")
	script := fmt.Sprintf("#!/bin/sh\nulimit -f 0\nexec '%s' \"$@\"\n", a.bin)
	if err := os.WriteFile(a.enospc, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	// One session, answered by llm-echo at once, whose title every Save
	// sets: it stays in history across walks, so a start after a lost
	// meta.json still lists it and the rename has a row.
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "hello")
	if err != nil {
		t.Fatal(err)
	}
	a.sid = row.ID
	if _, err := waitRow(s, a.sid, "the first turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init is a fresh install with serve down: no meta.json, no leftover,
// no token. History stays; with no meta.json it holds no titles.
func (a *mlcAdapter) Init() error {
	a.gate.reset()
	a.s.Shutdown()
	for _, p := range []string{a.meta, a.meta + ".tmp", a.token} {
		if err := os.Remove(p); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	a.s.Token = ""
	a.up, a.refused, a.title, a.good = false, false, "", nil
	return a.record("Init", "")
}

func (a *mlcAdapter) Cleanup() error { return nil }

func (a *mlcAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Serve", Index: 0}: a}, nil
}

func (a *mlcAdapter) GetState() (map[string]any, error) {
	st, err := a.files()
	if err != nil {
		return nil, err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	switch _, err := a.s.Build(ctx); {
	case err == nil:
		st["serve"] = "up"
	case a.refused:
		st["serve"] = "refused"
	default:
		st["serve"] = "down"
	}
	return st, nil
}

// files is every field the disk holds: meta, tmp, token, titled.
func (a *mlcAdapter) files() (map[string]any, error) {
	b, err := os.ReadFile(a.meta)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	meta := "absent"
	if err == nil {
		meta = metaShape(b)
	}
	_, terr := os.Stat(a.meta + ".tmp")
	tok, err := os.ReadFile(a.token)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	token := "absent"
	if err == nil {
		token = "empty"
		if len(bytes.TrimSpace(tok)) > 0 {
			token = "ok"
		}
	}
	return map[string]any{
		"meta":   meta,
		"tmp":    terr == nil,
		"token":  token,
		"titled": a.title != "" && bytes.Contains(b, []byte(a.title)),
	}, nil
}

// metaShape names what a meta.json holds, the way loadMeta would take
// it: a v2 file, the legacy bare map, one wrongly-typed field in either
// ("badfield"), torn JSON, or nothing.
func metaShape(b []byte) string {
	if len(b) == 0 {
		return "empty"
	}
	if !json.Valid(b) {
		return "torn"
	}
	var top map[string]json.RawMessage
	if json.Unmarshal(b, &top) != nil {
		return "not an object"
	}
	if _, ok := top["version"]; ok {
		var f struct {
			Version  int                          `json:"version"`
			Sessions map[string]serve.SessionMeta `json:"sessions"`
		}
		if json.Unmarshal(b, &f) != nil {
			return "badfield"
		}
		if f.Version == 2 {
			return "v2"
		}
		return fmt.Sprintf("v%d", f.Version)
	}
	var m map[string]serve.SessionMeta
	if json.Unmarshal(b, &m) != nil {
		return "badfield"
	}
	return "legacy"
}

// record journals a step with the files after it.
func (a *mlcAdapter) record(action, log string) error {
	f, err := a.files()
	if err != nil {
		return err
	}
	st := mlcStep{Action: action, Files: f, Log: log}
	st.Start = action == "Start" || action == "StartTokenENOSPC"
	if action == "Init" {
		a.journal = append(a.journal, nil)
	}
	w := len(a.journal) - 1
	a.journal[w] = append(a.journal[w], st)
	return nil
}

func (a *mlcAdapter) metaNow() string {
	b, err := os.ReadFile(a.meta)
	if err != nil {
		return "absent"
	}
	return metaShape(b)
}

func (a *mlcAdapter) tokenAbsent() bool {
	_, err := os.Stat(a.token)
	return errors.Is(err, os.ErrNotExist)
}

// launch is one start of bin: up when it answers, refused when it exits
// first (the error it printed is in its log).
func (a *mlcAdapter) launch(action, bin string) error {
	mark := len(a.s.Output())
	err := a.s.Resume(bin)
	log := a.s.Output()[mark:]
	switch {
	case err == nil:
		a.up, a.refused = true, false
	case strings.Contains(err.Error(), "exited before ready"):
		// Only a refusal over one of the two files is the spec's: a
		// taken port or a crash elsewhere is this harness failing.
		if !strings.Contains(log, "serve.token") && !strings.Contains(log, "meta.json") {
			return fmt.Errorf("serve exited, not over meta.json or the token: %s", log)
		}
		a.up, a.refused = false, true
	default:
		return err
	}
	return a.record(action, log)
}

// Start is serve starting, by hand or by launchd's KeepAlive.
func (a *mlcAdapter) Start() error {
	if !a.gate.pass(!a.up) {
		return nil
	}
	return a.launch("Start", a.bin)
}

func (a *mlcAdapter) StartTokenENOSPC() error {
	if !a.gate.pass(!a.up && a.tokenAbsent()) {
		return nil
	}
	if a.enospcSucceeds {
		return a.launch("StartTokenENOSPC", a.bin)
	}
	return a.launch("StartTokenENOSPC", a.enospc)
}

// Save is a person renaming the session: saveMetaLocked.
func (a *mlcAdapter) Save() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	a.n++
	title := fmt.Sprintf("kept title %d", a.n)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Rename(ctx, a.sid, title); err != nil {
		return err
	}
	a.title = title
	return a.record("Save", "")
}

// Stop is SIGTERM, as launchd and `bough update` send it.
func (a *mlcAdapter) Stop() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	a.s.Shutdown()
	a.up = false
	return a.record("Stop", "")
}

// kill is the power going: SIGKILL to serve's whole group, nothing
// flushed or cleaned up.
func (a *mlcAdapter) kill() {
	syscall.Kill(-a.s.PID(), syscall.SIGKILL)
	a.s.Shutdown() // returns once the process is reaped
	a.up = false
}

// PowerLossEmpty: the rename of a save reached the disk, its data did
// not. Whatever meta.json held is gone.
func (a *mlcAdapter) PowerLossEmpty() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	a.kill()
	if err := os.WriteFile(a.meta, nil, 0o644); err != nil {
		return err
	}
	return a.record("PowerLossEmpty", "")
}

// PowerLossBeforeRename: the save wrote part of the .tmp and never got
// to the rename.
func (a *mlcAdapter) PowerLossBeforeRename() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	a.kill()
	if err := os.WriteFile(a.meta+".tmp", []byte(`{"version":2,"sessions":{`), 0o644); err != nil {
		return err
	}
	return a.record("PowerLossBeforeRename", "")
}

// table reads the readable meta.json as a generic object and returns it
// with its sessions table (the whole object for the legacy map).
func (a *mlcAdapter) table() (top, rows map[string]any, err error) {
	b, err := os.ReadFile(a.meta)
	if err != nil {
		return nil, nil, err
	}
	if err := json.Unmarshal(b, &top); err != nil {
		return nil, nil, err
	}
	if _, ok := top["version"]; !ok {
		return top, top, nil
	}
	rows, _ = top["sessions"].(map[string]any)
	if rows == nil {
		rows = map[string]any{}
		top["sessions"] = rows
	}
	return top, rows, nil
}

func (a *mlcAdapter) writeJSON(v any) error {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(a.meta, append(b, '\n'), 0o644)
}

// keepGood remembers the rows a corrupting edit starts from, as a
// person has them in their head (or a backup) when they repair it.
func (a *mlcAdapter) keepGood() error {
	_, rows, err := a.table()
	if err != nil {
		return err
	}
	a.good = map[string]any{}
	for k, v := range rows {
		a.good[k] = v
	}
	return nil
}

func (a *mlcAdapter) readable() bool {
	m := a.metaNow()
	return !a.up && (m == "v2" || m == "legacy")
}

// EditTorn cuts the file's last brace: the titles are still in it, the
// JSON is not whole.
func (a *mlcAdapter) EditTorn() error {
	if !a.gate.pass(a.readable()) {
		return nil
	}
	if err := a.keepGood(); err != nil {
		return err
	}
	b, err := os.ReadFile(a.meta)
	if err != nil {
		return err
	}
	b = bytes.TrimRight(b, "\n")
	if err := os.WriteFile(a.meta, b[:len(b)-1], 0o644); err != nil {
		return err
	}
	return a.record("EditTorn", "")
}

// EditBadField makes the session's archived flag a string.
func (a *mlcAdapter) EditBadField() error {
	if !a.gate.pass(a.readable()) {
		return nil
	}
	if err := a.keepGood(); err != nil {
		return err
	}
	top, rows, err := a.table()
	if err != nil {
		return err
	}
	row, _ := rows[a.sid].(map[string]any)
	if row == nil {
		row = map[string]any{}
		rows[a.sid] = row
	}
	row["archived"] = "yes"
	if err := a.writeJSON(top); err != nil {
		return err
	}
	return a.record("EditBadField", "")
}

// EditLegacy puts back a file from before the projects table: the bare
// map of session id -> meta.
func (a *mlcAdapter) EditLegacy() error {
	if !a.gate.pass(!a.up && a.metaNow() == "v2") {
		return nil
	}
	_, rows, err := a.table()
	if err != nil {
		return err
	}
	if err := a.writeJSON(rows); err != nil {
		return err
	}
	return a.record("EditLegacy", "")
}

// RepairMeta is a human writing the rows back as a whole v2 file.
func (a *mlcAdapter) RepairMeta() error {
	m := a.metaNow()
	if !a.gate.pass(!a.up && (m == "torn" || m == "badfield")) {
		return nil
	}
	if err := a.writeJSON(map[string]any{"version": 2, "sessions": a.good}); err != nil {
		return err
	}
	return a.record("RepairMeta", "")
}

func (a *mlcAdapter) DeleteMeta() error {
	if !a.gate.pass(!a.up && a.metaNow() != "absent") {
		return nil
	}
	if err := os.Remove(a.meta); err != nil {
		return err
	}
	return a.record("DeleteMeta", "")
}

func (a *mlcAdapter) DeleteToken() error {
	if !a.gate.pass(!a.up && !a.tokenAbsent()) {
		return nil
	}
	if err := os.Remove(a.token); err != nil {
		return err
	}
	a.s.Token = "" // the next start mints another
	return a.record("DeleteToken", "")
}

// mlcAction counts an action that ran (the gate let it through).
func mlcAction(name string, f func(*mlcAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*mlcAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
		}
		return nil, err
	}
}

var mlcActions = map[string]map[string]fmbt.ActionFunc{"Serve": {
	"Start":                 mlcAction("Start", (*mlcAdapter).Start),
	"StartTokenENOSPC":      mlcAction("StartTokenENOSPC", (*mlcAdapter).StartTokenENOSPC),
	"Save":                  mlcAction("Save", (*mlcAdapter).Save),
	"Stop":                  mlcAction("Stop", (*mlcAdapter).Stop),
	"PowerLossEmpty":        mlcAction("PowerLossEmpty", (*mlcAdapter).PowerLossEmpty),
	"PowerLossBeforeRename": mlcAction("PowerLossBeforeRename", (*mlcAdapter).PowerLossBeforeRename),
	"EditTorn":              mlcAction("EditTorn", (*mlcAdapter).EditTorn),
	"EditBadField":          mlcAction("EditBadField", (*mlcAdapter).EditBadField),
	"EditLegacy":            mlcAction("EditLegacy", (*mlcAdapter).EditLegacy),
	"RepairMeta":            mlcAction("RepairMeta", (*mlcAdapter).RepairMeta),
	"DeleteMeta":            mlcAction("DeleteMeta", (*mlcAdapter).DeleteMeta),
	"DeleteToken":           mlcAction("DeleteToken", (*mlcAdapter).DeleteToken),
}}

// Init is a shutdown and three unlinks, and a start is one serve boot,
// so random walks are cheap; they run only in the exhaustive job.
func mlcOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 8, "max-parallel-runs": 0}
}

// walkMLCPaths walks every generated path for cover and compares the
// adapter's state with the spec's at each step. Every path runs, so one
// run reports every divergence.
func walkMLCPaths(a *mlcAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("meta_load_corrupt", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Serve#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *mlcAdapter) walk(trace []tracecheck.Step) error {
	for j, s := range trace {
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else {
			name := strings.TrimPrefix(s.Action, "Serve#0.")
			_, err = mlcActions["Serve"][name](a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter found it disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		if d := mlcDiff(s.State, got); d != "" {
			return fmt.Errorf("step %d (%s): %s", j, s.Action, d)
		}
	}
	return nil
}

func mlcDiff(want, got map[string]any) string {
	var d []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Serve#0.")
		if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
			d = append(d, fmt.Sprintf("%s: spec %v, serve %v", f, v, got[f]))
		}
	}
	sort.Strings(d)
	if d == nil {
		return ""
	}
	return strings.Join(d, "; ") + fmt.Sprintf(" (state %v)", got)
}

// mlcHistory reads a walk's journal as the spec's steps. serve is taken
// from each start's own log, not from the health probe GetState uses:
// the listening line means up, anything else (the error that ended the
// process) is a refusal. That log is all launchd keeps of a refusal.
func mlcHistory(walk []mlcStep) []tracecheck.Step {
	var steps []tracecheck.Step
	for _, s := range walk {
		st := map[string]any{}
		for k, v := range s.Files {
			st["Serve#0."+k] = v
		}
		if s.Start {
			st["Serve#0.serve"] = "refused"
			if strings.Contains(s.Log, "bough serve: http://") {
				st["Serve#0.serve"] = "up"
			}
		}
		action := s.Action
		if action != "Init" {
			action = "Serve#0." + action
		}
		steps = append(steps, tracecheck.Step{Action: action, State: st})
	}
	return steps
}

func checkMLCJournal(t *testing.T, a *mlcAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "meta_load_corrupt"))
	if err != nil {
		t.Fatal(err)
	}
	starts := 0
	for i, w := range a.journal {
		steps := mlcHistory(w)
		for _, s := range w {
			if s.Start {
				starts++
			}
		}
		if v := g.Check(steps); v != nil {
			b, _ := json.Marshal(steps)
			t.Errorf("walk %d's serve log is not a path in the model: %v\ntrace: %s", i, v, b)
		}
	}
	t.Logf("trace-checked %d walks, %d starts read off serve's log", len(a.journal), starts)
}

// TestMetaLoadCorrupt lets fizzbee-mbt walk the spec at random.
func TestMetaLoadCorrupt(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMLCAdapter(t)
	if err := runMBT(t, "meta_load_corrupt", a, mlcActions, mlcOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkMLCJournal(t, a)
}

// TestMetaLoadCorruptPaths walks every generated path against a real
// serve and replays each walk's serve log on the graph.
func TestMetaLoadCorruptPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMLCAdapter(t)
	if err := walkMLCPaths(a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkMLCJournal(t, a)
}

// The projection is only a check if a log the model forbids is refused:
// a start that came up on a torn meta.json.
func TestMetaLoadCorruptHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "meta_load_corrupt"))
	if err != nil {
		t.Fatal(err)
	}
	files := func(meta, token string, titled bool) map[string]any {
		return map[string]any{"meta": meta, "tmp": false, "token": token, "titled": titled}
	}
	up := "bough serve: http://127.0.0.1:1 (pid 1)\n"
	refused := "bough: serve: supervisor: serve: supervisor: parse meta.json: unexpected end of JSON input\n"
	walk := func(last string) []mlcStep {
		return []mlcStep{
			{Action: "Init", Files: files("absent", "absent", false)},
			{Action: "Start", Files: files("absent", "ok", false), Start: true, Log: up},
			{Action: "Save", Files: files("v2", "ok", true)},
			{Action: "Stop", Files: files("v2", "ok", true)},
			{Action: "EditTorn", Files: files("torn", "ok", true)},
			{Action: "Start", Files: files("torn", "ok", true), Start: true, Log: last},
		}
	}
	if v := g.Check(mlcHistory(walk(refused))); v != nil {
		t.Fatalf("a refused start on a torn file: %v", v)
	}
	if v := g.Check(mlcHistory(walk(up))); v == nil {
		t.Fatal("a start that listened on a torn meta.json passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run whose ENOSPC start writes the token after all must fail, or a
// green TestMetaLoadCorruptPaths proves nothing. The empty token is
// reachable only through that one transition, so every state is enough
// to take it.
func TestMetaLoadCorruptCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMLCAdapter(t)
	a.enospcSucceeds = true
	err := walkMLCPaths(a, tracecheck.CoverStates)
	if err == nil {
		t.Fatal("a run whose StartTokenENOSPC starts serve cleanly passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
