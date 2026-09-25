//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
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

// specs/history_io_failure.fizz against a real serve: one web session,
// its child, its history file and what serve answers about it.
//
// The disk is BOUGH_TEST_HISTORY_FAULT_DIR: while <dir>/full exists every
// write the child's history store attempts fails with ENOSPC, and the
// store's retry tick fires when <dir>/<id>.flush appears (the test's
// clock), so a freed disk stays pending until the spec's Flush. Tearing,
// corrupting and chmodding the file are done to the file itself, through
// a descriptor the adapter opened before any chmod, so it can still read
// the file while serve cannot.
//
// The page's view (list, open) is the adapter's: it changes only when the
// spec's ListPoll or OpenSession asks serve with the fault in place. Every
// other field is read off serve and the file with the fault lifted for
// the read, since it is not about whether serve can read the file.

const (
	hioTear    = `{"kind":"hio-torn-fragment","data":{"text":"cut`
	hioCorrupt = `{"kind":"hio-corrupt" not json`
	hioHuge    = "hio-huge "
)

type historyIOAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	seam string // BOUGH_TEST_HISTORY_FAULT_DIR
	gate gate

	id, cwd string
	f       *os.File // the session file, opened before any fault
	ev      *eventLog
	n       int
	held    string // the running turn's name
	// want is the latest entry the child owes the file: the input of
	// wantName ("input") or the done after it ("done").
	want, wantName string

	disk, turn, fault, list, open string

	ids   []string
	taken map[string]int

	// fillsNothing is TestHistoryIoFailureCatchesWrongAdapter's bug:
	// DiskFills never fills the disk.
	fillsNothing bool
}

func newHistoryIOAdapter(t *testing.T) *historyIOAdapter {
	seam := t.TempDir()
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_TEST_HISTORY_FAULT_DIR=" + seam}})
	a := &historyIOAdapter{t: t, s: s, dir: control.Dir(s.Home), seam: seam, taken: map[string]int{}}
	t.Cleanup(func() { a.Cleanup() })
	return a
}

func (a *historyIOAdapter) histDir() string { return filepath.Join(a.s.Home, ".bough", "history") }
func (a *historyIOAdapter) path() string    { return filepath.Join(a.histDir(), a.id+".jsonl") }
func (a *historyIOAdapter) full() string    { return filepath.Join(a.seam, "full") }

func (a *historyIOAdapter) next(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

// Init starts each walk on a fresh session whose first turn finished:
// the child is alive and idle, and the file ends in its done.
func (a *historyIOAdapter) Init() error {
	a.gate.reset()
	if err := a.Cleanup(); err != nil {
		return err
	}
	name := a.next("i")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, "w"+name)
	row, err := a.s.CreateSession(ctx, a.cwd, "turn "+name)
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.want, a.wantName = "done", name
	a.disk, a.turn, a.fault, a.list, a.open = "ok", "done", "none", "listed", "shown"
	if _, err := waitRow(a.s, a.id, "the first turn's done", func(r serve.Row) bool { return r.Live && r.Status == serve.StatusDone }); err != nil {
		return err
	}
	if a.f, err = os.OpenFile(a.path(), os.O_RDWR|os.O_APPEND, 0); err != nil {
		return err
	}
	if a.ev, err = openEventLog(a.s, a.id); err != nil {
		return err
	}
	return a.quiet()
}

// quiet waits until the file stops changing: a turn's title and summary
// land a moment after its done, and a write racing the next DiskFills
// would be a step nobody took.
func (a *historyIOAdapter) quiet() error {
	var last []byte
	stable := 0
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		b, err := a.read()
		if err != nil {
			return err
		}
		if bytes.Equal(b, last) {
			if stable++; stable >= 5 {
				return nil
			}
		} else {
			last, stable = b, 0
		}
		time.Sleep(40 * time.Millisecond)
	}
	return errors.New("the session file never went quiet")
}

// Cleanup puts the disk and the file back, and kills the walk's child
// so no walk's retries or late writes reach the next one.
func (a *historyIOAdapter) Cleanup() error {
	a.lift()
	os.Remove(a.full())
	if a.ev != nil {
		a.ev.close()
		a.ev = nil
	}
	if a.f != nil {
		a.f.Close()
		a.f = nil
	}
	a.held = ""
	if a.cwd != "" {
		killCwd(a.cwd)
		a.cwd = ""
	}
	return nil
}

// lift undoes a chmod fault; restore puts it back.
func (a *historyIOAdapter) lift() {
	switch a.fault {
	case "file":
		os.Chmod(a.path(), 0o644)
	case "dir":
		os.Chmod(a.histDir(), 0o755)
	}
}

func (a *historyIOAdapter) restore() {
	switch a.fault {
	case "file":
		os.Chmod(a.path(), 0)
	case "dir":
		os.Chmod(a.histDir(), 0)
	}
}

// killCwd SIGKILLs the bough process working in cwd: a crash, so
// nothing the child held reaches its file.
func killCwd(cwd string) {
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
}

func (a *historyIOAdapter) read() ([]byte, error) {
	st, err := a.f.Stat()
	if err != nil {
		return nil, err
	}
	b := make([]byte, st.Size())
	if _, err := a.f.ReadAt(b, 0); err != nil {
		return nil, err
	}
	return b, nil
}

// rewrite replaces the file's content in place (same inode, so the
// child's descriptor and serve's path keep naming it), under the lock
// the child's appends take.
func (a *historyIOAdapter) rewrite(edit func([]byte) []byte) error {
	fd := int(a.f.Fd())
	if err := syscall.Flock(fd, syscall.LOCK_EX); err != nil {
		return err
	}
	defer syscall.Flock(fd, syscall.LOCK_UN)
	b, err := a.read()
	if err != nil {
		return err
	}
	if err := a.f.Truncate(0); err != nil {
		return err
	}
	_, err = a.f.Write(edit(b))
	return err
}

// hioFile is what the adapter reads off the session file.
type hioFile struct {
	lines   [][]byte // newline-terminated lines, without the newline
	tail    []byte   // what follows the last newline
	entries []history.Entry
}

func parseHIO(b []byte) hioFile {
	var f hioFile
	for {
		i := bytes.IndexByte(b, '\n')
		if i < 0 {
			f.tail = b
			break
		}
		f.lines = append(f.lines, b[:i])
		b = b[i+1:]
	}
	for _, l := range f.lines {
		var e history.Entry
		if json.Unmarshal(l, &e) == nil {
			f.entries = append(f.entries, e)
		}
	}
	return f
}

func (a *historyIOAdapter) file() (hioFile, error) {
	b, err := a.read()
	return parseHIO(b), err
}

// pending: the file lacks the latest entry the child owes it.
func (a *historyIOAdapter) pending(f hioFile) bool {
	in := slices.IndexFunc(f.entries, func(e history.Entry) bool {
		return e.Kind == "input" && history.Prompt(e) == "turn "+a.wantName
	})
	if in < 0 {
		return true
	}
	if a.want == "input" {
		return false
	}
	return !slices.ContainsFunc(f.entries[in+1:], func(e history.Entry) bool { return e.Kind == "done" })
}

func ondisk(f hioFile) string {
	st := "closed"
	for _, e := range f.entries {
		switch e.Kind {
		case "input":
			st = "open"
		case "done", "cancelled":
			st = "closed"
		}
	}
	return st
}

// row is the session's row as serve holds it, the fault lifted.
func (a *historyIOAdapter) row() (serve.Row, error) {
	a.lift()
	defer a.restore()
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	return row, err
}

func (a *historyIOAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *historyIOAdapter) GetState() (map[string]any, error) {
	f, err := a.file()
	if err != nil {
		return nil, err
	}
	row, err := a.row()
	if err != nil {
		return nil, err
	}
	child := "gone"
	if row.Live {
		child = "alive"
	}
	tail := "clean"
	if len(f.tail) > 0 {
		tail = "torn"
	}
	return map[string]any{
		"disk":    a.disk,
		"child":   child,
		"turn":    a.turn,
		"ondisk":  ondisk(f),
		"tail":    tail,
		"glued":   slices.ContainsFunc(f.lines, func(l []byte) bool { return bytes.Contains(l, []byte(hioTear)) }),
		"pending": a.pending(f),
		"warn":    row.Unsaved,
		"fault":   a.fault,
		"list":    a.list,
		"open":    a.open,
		"status":  string(row.Status),
	}, nil
}

// settleWarn gives serve a moment to read the child's history event
// after a write it cannot otherwise wait on; a warn that never comes is
// left for the state check to report.
func (a *historyIOAdapter) settleWarn(want bool) {
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if row, err := a.row(); err == nil && row.Unsaved == want {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *historyIOAdapter) DiskFills() error {
	if !a.gate.pass(a.disk == "ok") {
		return nil
	}
	a.disk = "full"
	if a.fillsNothing {
		return nil
	}
	return os.WriteFile(a.full(), nil, 0o644)
}

func (a *historyIOAdapter) DiskFrees() error {
	if !a.gate.pass(a.disk == "full") {
		return nil
	}
	a.disk = "ok"
	if err := os.Remove(a.full()); err != nil && !(a.fillsNothing && errors.Is(err, os.ErrNotExist)) {
		return err
	}
	return nil
}

// Tear leaves a fragment with no newline at the end of the file, as a
// write cut part way (ENOSPC mid-line, or a crash) does.
func (a *historyIOAdapter) Tear() error {
	f, err := a.file()
	if err != nil {
		return err
	}
	if !a.gate.pass(len(f.tail) == 0) {
		return nil
	}
	return a.rewrite(func(b []byte) []byte { return append(b, hioTear...) })
}

// Prompt is the composer. A gone child is resumed by serve.
func (a *historyIOAdapter) Prompt() error {
	if !a.gate.pass(a.turn != "running" && (a.fault == "none" || a.fault == "corrupt" || a.fault == "oversize")) {
		return nil
	}
	name := a.next("t")
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	a.held, a.turn, a.want, a.wantName = name, "running", "input", name
	// The engine records the input before it asks the model.
	if err := waitTaken(a.dir, name); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the child", func(r serve.Row) bool { return r.Live }); err != nil {
		return err
	}
	a.settleWarn(a.disk == "full")
	return nil
}

// Finish releases the held turn and waits for the child's done event:
// the done entry was written (or failed to be) before it.
func (a *historyIOAdapter) Finish() error {
	if !a.gate.pass(a.turn == "running") {
		return nil
	}
	mark := a.ev.lastSeq()
	control.Release(a.t, a.dir, a.held)
	a.held, a.turn, a.want = "", "done", "done"
	if err := a.ev.waitKind("done", mark); err != nil {
		return err
	}
	a.settleWarn(a.disk == "full")
	return nil
}

// Flush is the store's retry tick, fired by the test's clock.
func (a *historyIOAdapter) Flush() error {
	f, err := a.file()
	if err != nil {
		return err
	}
	row, err := a.row()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live && a.pending(f) && a.disk == "ok") {
		return nil
	}
	tick := filepath.Join(a.seam, a.id+".flush")
	if err := os.WriteFile(tick, nil, 0o644); err != nil {
		return err
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(tick); errors.Is(err, os.ErrNotExist) {
			break
		}
		if time.Now().After(deadline) {
			os.Remove(tick)
			return errors.New("the child never took the retry tick")
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.settleWarn(false)
	return nil
}

// Exit is the child crashing: SIGKILL, so nothing pending is written.
func (a *historyIOAdapter) Exit() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live) {
		return nil
	}
	killCwd(a.cwd)
	a.held = ""
	if a.turn == "running" {
		a.turn = "interrupted"
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		row, err := a.row()
		if err != nil {
			return err
		}
		if !row.Live {
			return nil
		}
		if time.Now().After(deadline) {
			return errors.New("the child is still live after SIGKILL")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *historyIOAdapter) seen() bool {
	return a.fault == "none" && a.list == "listed" && a.open == "shown"
}

// WriteHugeEntry puts one entry over bufio.Scanner's 4 MiB line cap in
// the file, the way the child records a huge tool result: a whole line,
// before any torn fragment (which is the child's own later write cut).
func (a *historyIOAdapter) WriteHugeEntry() error {
	if !a.gate.pass(a.turn == "running" && a.disk == "ok" && a.seen()) {
		return nil
	}
	a.fault = "oversize"
	return a.rewrite(func(b []byte) []byte {
		f := parseHIO(b)
		var seq int64
		for _, e := range f.entries {
			seq = max(seq, e.Seq)
		}
		line, _ := json.Marshal(history.Entry{Seq: seq + 1, At: time.Now(), Kind: "notice", Data: map[string]any{"text": hioHuge + strings.Repeat("x", 4<<20+1024)}})
		head := b[:len(b)-len(f.tail)]
		out := append(slices.Clone(head), line...)
		out = append(out, '\n')
		return append(out, f.tail...)
	})
}

// CorruptLine puts a line that is not JSON right after the first one.
func (a *historyIOAdapter) CorruptLine() error {
	if !a.gate.pass(a.seen()) {
		return nil
	}
	a.fault = "corrupt"
	return a.rewrite(func(b []byte) []byte {
		i := bytes.IndexByte(b, '\n') + 1
		out := slices.Clone(b[:i])
		out = append(out, hioCorrupt+"\n"...)
		return append(out, b[i:]...)
	})
}

func (a *historyIOAdapter) Chmod000() error {
	if !a.gate.pass(a.seen()) {
		return nil
	}
	a.fault = "file"
	a.restore()
	return nil
}

func (a *historyIOAdapter) ChmodDir() error {
	if !a.gate.pass(a.seen()) {
		return nil
	}
	a.fault = "dir"
	a.restore()
	return nil
}

// Repair chmods back, or deletes the bad line or the huge entry.
func (a *historyIOAdapter) Repair() error {
	if !a.gate.pass(a.fault != "none") {
		return nil
	}
	a.lift()
	fault := a.fault
	a.fault = "none"
	drop := ""
	switch fault {
	case "corrupt":
		drop = hioCorrupt
	case "oversize":
		drop = hioHuge
	default:
		return nil
	}
	return a.rewrite(func(b []byte) []byte {
		f := parseHIO(b)
		var out []byte
		for _, l := range f.lines {
			if !bytes.Contains(l, []byte(drop)) {
				out = append(append(out, l...), '\n')
			}
		}
		return append(out, f.tail...)
	})
}

// ListPoll is the page's list poll, with the fault in place.
func (a *historyIOAdapter) ListPoll() error {
	if !a.gate.pass(true) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, true)
	var ae *servetest.APIError
	switch {
	case errors.As(err, &ae) && ae.Status == 500:
		a.list = "error"
	case err != nil:
		return err
	case slices.ContainsFunc(rows, func(r serve.Row) bool { return r.ID == a.id }):
		a.list = "listed"
	case len(rows) == 0:
		a.list = "empty"
	default:
		a.list = "missing"
	}
	return nil
}

// OpenSession is the page opening the session, with the fault in place:
// "partial" when the transcript lacks the file's last input.
func (a *historyIOAdapter) OpenSession() error {
	if !a.gate.pass(true) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	var ae *servetest.APIError
	switch {
	case errors.As(err, &ae) && ae.Status == 404:
		a.open = "missing"
	case errors.As(err, &ae) && ae.Status == 500 && strings.Contains(ae.Msg, "too long"):
		a.open = "toolong"
	case errors.As(err, &ae) && ae.Status == 500:
		a.open = "error"
	case err != nil:
		return err
	default:
		f, err := a.file()
		if err != nil {
			return err
		}
		last := int64(-1)
		for _, e := range f.entries {
			if e.Kind == "input" {
				last = e.Seq
			}
		}
		a.open = "partial"
		if slices.ContainsFunc(lines, func(l serve.Line) bool { return l.Seq == last }) {
			a.open = "shown"
		}
	}
	return nil
}

// eventLog keeps every event of the walk's session stream, read by a
// goroutine so serve never drops the subscriber as stalled.
type eventLog struct {
	mu  sync.Mutex
	evs []serve.Event
	st  *servetest.Stream
}

func openEventLog(s *servetest.Server, id string) (*eventLog, error) {
	// Not actionCtx: the stream lives for the walk, and close ends it.
	st, err := s.Events(context.Background(), id)
	if err != nil {
		return nil, err
	}
	l := &eventLog{st: st}
	go func() {
		for {
			ev, err := st.Next()
			if err != nil {
				return
			}
			l.mu.Lock()
			l.evs = append(l.evs, ev)
			l.mu.Unlock()
		}
	}()
	return l, nil
}

func (l *eventLog) lastSeq() int64 {
	l.mu.Lock()
	defer l.mu.Unlock()
	var n int64
	for _, e := range l.evs {
		n = max(n, e.Seq)
	}
	return n
}

func (l *eventLog) waitKind(kind string, after int64) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		l.mu.Lock()
		ok := slices.ContainsFunc(l.evs, func(e serve.Event) bool { return e.Kind == kind && e.Seq > after })
		l.mu.Unlock()
		if ok {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("no %q event after seq %d", kind, after)
}

func (l *eventLog) close() { l.st.Close() }

func hioAction(name string, f func(*historyIOAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*historyIOAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
		}
		return nil, err
	}
}

var historyIOActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"DiskFills":      hioAction("DiskFills", (*historyIOAdapter).DiskFills),
	"DiskFrees":      hioAction("DiskFrees", (*historyIOAdapter).DiskFrees),
	"Tear":           hioAction("Tear", (*historyIOAdapter).Tear),
	"Prompt":         hioAction("Prompt", (*historyIOAdapter).Prompt),
	"Finish":         hioAction("Finish", (*historyIOAdapter).Finish),
	"Flush":          hioAction("Flush", (*historyIOAdapter).Flush),
	"Exit":           hioAction("Exit", (*historyIOAdapter).Exit),
	"WriteHugeEntry": hioAction("WriteHugeEntry", (*historyIOAdapter).WriteHugeEntry),
	"CorruptLine":    hioAction("CorruptLine", (*historyIOAdapter).CorruptLine),
	"Chmod000":       hioAction("Chmod000", (*historyIOAdapter).Chmod000),
	"ChmodDir":       hioAction("ChmodDir", (*historyIOAdapter).ChmodDir),
	"Repair":         hioAction("Repair", (*historyIOAdapter).Repair),
	"ListPoll":       hioAction("ListPoll", (*historyIOAdapter).ListPoll),
	"OpenSession":    hioAction("OpenSession", (*historyIOAdapter).OpenSession),
}}

// Fifteen actions, most enabled at once; a walk of 10 costs a turn or
// two and maybe a resume.
func historyIOOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 10, "max-parallel-runs": 0}
}

// historyIOHistory reads a transcript as the spec's steps. The first
// turn is Init's; later inputs are Prompt, dones Finish, and the
// "cancelled" a resume writes over a turn its dead child left open is
// the Exit before that Prompt. The disk, faults and the page leave no
// trace in the file, and none is needed: only the turn is checked.
func historyIOHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...string) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Session#0."+kv[i]] = kv[i+1]
		}
		return m
	}
	var steps []tracecheck.Step
	inits := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if inits {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: q("turn", "running")})
			}
		case "done":
			if !inits {
				inits = true
				steps = append(steps, tracecheck.Step{Action: "Init", State: q("turn", "done", "ondisk", "closed")})
				continue
			}
			steps = append(steps, tracecheck.Step{Action: "Session#0.Finish", State: q("turn", "done")})
		case "cancelled":
			if inits {
				steps = append(steps, tracecheck.Step{Action: "Session#0.Exit", State: q("child", "gone", "turn", "interrupted")})
			}
		}
	}
	return steps
}

func init() { historyProjections["history_io_failure"] = historyIOHistory }

// walkHistoryIOPaths walks every generated path and compares the state
// after each step. stopFirst ends the run at the first divergence (the
// wrong-adapter check needs one); otherwise each path runs to its first.
func walkHistoryIOPaths(t *testing.T, a *historyIOAdapter, cover tracecheck.Cover, stopFirst bool) error {
	t.Helper()
	b, err := pathsJSONCover("history_io_failure", cover)
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
			errs = append(errs, fmt.Errorf("path %d: %w", i, err))
			if stopFirst {
				break
			}
		}
	}
	t.Logf("%d paths, actions taken: %v", len(doc.Paths), a.taken)
	return errors.Join(errs...)
}

func (a *historyIOAdapter) walk(trace []tracecheck.Step) error {
	var did []string
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "Session#0.")
		did = append(did, name)
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = historyIOActions["Session"][name](a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter found it disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w; walked %v", j, name, err, did)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w; walked %v", j, name, err, did)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Session#0.")
			if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s; walked %v; file %s", j, name, strings.Join(diff, "; "), did, a.kinds())
		}
	}
	return nil
}

// kinds is the file's entries as "seq:kind" (and the text of inputs),
// for a failure message.
func (a *historyIOAdapter) kinds() string {
	f, err := a.file()
	if err != nil {
		return err.Error()
	}
	var out []string
	for _, e := range f.entries {
		k := fmt.Sprintf("%d:%s", e.Seq, e.Kind)
		if e.Kind == "input" {
			k += "(" + history.Prompt(e) + ")"
		}
		if t, _ := e.Data["text"].(string); e.Kind == "assistant" && len(t) < 80 {
			k += "(" + t + ")"
		}
		out = append(out, k)
	}
	return fmt.Sprintf("%v tail %q", out, f.tail)
}

func checkHistoryIOHistories(t *testing.T, a *historyIOAdapter) {
	t.Helper()
	a.Cleanup()
	g, err := tracecheck.Load(fizzCheck(t, "history_io_failure"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), historyIOHistory)
	}
	t.Logf("trace-checked %d sessions' histories", len(a.ids))
}

// TestHistoryIoFailure lets fizzbee-mbt walk the spec at random (the
// exhaustive run only; see runMBT).
func TestHistoryIoFailure(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryIOAdapter(t)
	if err := runMBT(t, "history_io_failure", a, historyIOActions, historyIOOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("actions taken: %v", a.taken)
	checkHistoryIOHistories(t, a)
}

// TestHistoryIoFailurePaths walks the generated paths against one serve.
func TestHistoryIoFailurePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryIOAdapter(t)
	if err := walkHistoryIOPaths(t, a, envCover(), false); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkHistoryIOHistories(t, a)
}

// The projection is a check only if a transcript the model forbids is
// refused: a second done with no input between.
func TestHistoryIoFailureHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "history_io_failure"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	ok := []history.Entry{e("meta"), e("input"), e("done"), e("input"), e("cancelled"), e("input"), e("done")}
	if v := g.Check(historyIOHistory(ok)); v != nil {
		t.Fatalf("a crash and resume: %v", v)
	}
	bad := []history.Entry{e("meta"), e("input"), e("done"), e("done")}
	if v := g.Check(historyIOHistory(bad)); v == nil {
		t.Fatal("a done with no turn open passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run whose DiskFills never fills the disk must fail, or a green
// TestHistoryIoFailurePaths proves nothing.
func TestHistoryIoFailureCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryIOAdapter(t)
	a.fillsNothing = true
	err := walkHistoryIOPaths(t, a, tracecheck.CoverStates, true)
	if err == nil {
		t.Fatal("a run whose DiskFills does nothing passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
