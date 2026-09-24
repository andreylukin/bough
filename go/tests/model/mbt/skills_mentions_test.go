//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
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

// specs/skills_mentions.fizz against a real serve: the composer's Skills
// button and its / and @ mentions for one open session. Most of the
// Composer role is the page's own state (which popover is up, the
// draft), which this adapter plays the way the page does. What only the
// server can answer is read off it:
//
//   - listed: whose skills GET /api/skills?session=<id> returned. The
//     session works in a repo with its own .claude/skills, serve runs
//     from HOME, and off.yml switches one home skill off, so a catalogue
//     read against serve's cwd, or one that keeps an off skill, is told
//     apart from the session's.
//   - sent and ran: off the session's transcript after Send. A skill ran
//     when the input the child submitted carries its "[skill: name]"
//     block, which only the child's commands registry puts there.
//   - an @ mention's rows: GET /api/files?session=<id>&q=<token>.
//
// A failed read (PickerFailed, MentionFailed) is the server refusing the
// request: the adapter sends it with a wrong token, the one failure a
// page can get from a healthy serve without a fault hook in the product.

// The fixture's skills: one per pool the catalogue must merge or drop.
const (
	smHomeSkill = "alpha-home" // ~/.claude/skills: every session has it
	smOffSkill  = "beta-off"   // ~/.claude/skills, switched off in off.yml
	smRepoSkill = "gamma-repo" // <session cwd>/.claude/skills
	smFile      = "notes.txt"  // in the session's cwd, for @
	smPath      = "/tmp/x"     // the leading path TypePath types
)

type skillsMentionsAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	work string // every walk's session cwd
	gate gate

	id    string
	ids   []string
	turn  int
	names []string // the catalogue the page holds; nil = not cached
	files []string // the @ picker's rows for its token

	// The Composer role, as the page holds it.
	picker, mention, kind, listed string
	cached, typedPath, sent       bool
	draft                         string
	tokAt, tokEnd                 int // the mention token is draft[tokAt:tokEnd]
	lead                          string

	// noSession is the deliberate bug TestSkillsMentionsCatchesWrongAdapter
	// injects: the catalogue is read without naming the session, which is
	// what the page did before /api/skills took one.
	noSession bool

	// What the checked part of the walks reached, logged at the end: a
	// green run that never loaded a catalogue or sent a skill says little.
	loads, sends map[string]int
}

func newSkillsMentionsAdapter(t *testing.T) *skillsMentionsAdapter {
	skill := func(name string) string {
		return "---\ndescription: The " + name + " fixture skill.\n---\nfixture body of " + name + "\n"
	}
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Files: map[string]string{
		".claude/skills/" + smHomeSkill + "/SKILL.md": skill(smHomeSkill),
		".claude/skills/" + smOffSkill + "/SKILL.md":  skill(smOffSkill),
		".bough/off.yml": "disabled:\n  - skill:" + smOffSkill + "\n",
	}})
	work := s.Dir(t, "work")
	for rel, body := range map[string]string{
		".claude/skills/" + smRepoSkill + "/SKILL.md": skill(smRepoSkill),
		smFile: "plain words\n",
	} {
		p := filepath.Join(work, rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return &skillsMentionsAdapter{t: t, s: s, dir: control.Dir(s.Home), work: work,
		loads: map[string]int{}, sends: map[string]int{}}
}

// Init starts a walk on a session nothing has been sent to. One is
// reused until a walk sends: the runner stops checking a walk at its
// first disabled action, so most walks are a step or two long, and a
// session per walk made them too slow to run the thousands it takes to
// reach a loaded catalogue at all often.
func (a *skillsMentionsAdapter) Init() error {
	if a.id == "" || a.sent {
		ctx, cancel := actionCtx()
		defer cancel()
		row, err := a.s.CreateSession(ctx, a.work, "")
		if err != nil {
			return err
		}
		a.id = row.ID
		a.ids = append(a.ids, row.ID)
	}
	a.names = nil
	a.picker, a.mention, a.kind, a.listed, a.lead = "closed", "none", "", "", "none"
	a.cached, a.typedPath, a.sent = false, false, false
	a.draft, a.tokAt, a.tokEnd = "", 0, 0
	a.gate.reset()
	return nil
}

// Cleanup has nothing to release: Send waits for its turn to end.
func (a *skillsMentionsAdapter) Cleanup() error { return nil }

func (a *skillsMentionsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Composer", Index: 0}: a}, nil
}

// GetState reads sent and ran off the session's transcript on every
// step, so a message the page never sent, or a skill that ran without
// one, shows up where it happened.
func (a *skillsMentionsAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	sent, ran := smTranscript(lines)
	return map[string]any{
		"picker": a.picker, "mention": a.mention, "kind": a.kind,
		"cached": a.cached, "listed": a.listed, "lead": a.lead,
		"hasPath": a.typedPath && strings.Contains(a.draft, smPath), "typedPath": a.typedPath,
		"sent": sent, "ran": ran,
	}, nil
}

// smTranscript: sent is a line the person sent reaching the session (the
// child records a "/" line as a command, anything else as input); ran is
// an input carrying an injected skill.
func smTranscript(lines []serve.Line) (sent, ran bool) {
	for _, l := range lines {
		switch l.Kind {
		case "command":
			sent = true
		case "input":
			sent = true
			if strings.Contains(l.Text, "\n[skill: ") {
				ran = true
			}
		}
	}
	return sent, ran
}

// get is one read the page makes. ok=false sends a wrong token, which a
// healthy serve refuses: the failure the Failed actions stand for.
func (a *skillsMentionsAdapter) get(path string, ok bool, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return err
	}
	tok := a.s.Token
	if !ok {
		tok = "not-" + tok
	}
	req.Header.Set("Authorization", "Bearer "+tok)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("GET %s: %s", path, resp.Status)
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// loadSkills is the catalogue read both pickers share (mention.tsx
// `catalogue`). listed says whose it was: "session" when it has the
// home skill and the session repo's, and not the one off.yml switches
// off; else the names it did return, which is what a mismatch should
// show. Serve seeds skills of its own under HOME (orb), so the check
// is on the fixture's three, not on the whole list.
func (a *skillsMentionsAdapter) loadSkills() error {
	path := "/api/skills?session=" + url.QueryEscape(a.id)
	if a.noSession {
		path = "/api/skills"
	}
	var body struct {
		Skills []struct {
			Name string `json:"name"`
		} `json:"skills"`
	}
	if err := a.get(path, true, &body); err != nil {
		return err
	}
	var names []string
	for _, s := range body.Skills {
		names = append(names, s.Name)
	}
	a.names, a.cached = names, true
	a.loads[a.picker+"/"+a.kind]++
	a.listed = "session"
	if !slices.Contains(names, smHomeSkill) || !slices.Contains(names, smRepoSkill) || slices.Contains(names, smOffSkill) {
		a.listed = "other: " + strings.Join(names, ",")
	}
	return nil
}

// failRead makes the read and requires the server to refuse it.
func (a *skillsMentionsAdapter) failRead(path string) error {
	var sink any
	if err := a.get(path, false, &sink); err == nil {
		return fmt.Errorf("GET %s with a wrong token succeeded", path)
	}
	return nil
}

func (a *skillsMentionsAdapter) dropCache() { a.names, a.cached, a.listed = nil, false, "" }

// --- the Skills button ---------------------------------------------------

func (a *skillsMentionsAdapter) OpenPicker() error {
	if !a.gate.pass(a.picker == "closed" && !a.sent) {
		return nil
	}
	a.closeMention()
	a.picker = "loading"
	if a.cached {
		a.picker = "loaded"
	}
	return nil
}

func (a *skillsMentionsAdapter) ClosePicker() error {
	if a.gate.pass(a.picker != "closed") {
		a.picker = "closed"
	}
	return nil
}

func (a *skillsMentionsAdapter) RetryPicker() error {
	if a.gate.pass(a.picker == "error") {
		a.picker = "loading"
	}
	return nil
}

func (a *skillsMentionsAdapter) PickerLoaded() error {
	if !a.gate.pass(a.picker == "loading") {
		return nil
	}
	if err := a.loadSkills(); err != nil {
		return err
	}
	a.picker = "loaded"
	return nil
}

func (a *skillsMentionsAdapter) PickerFailed() error {
	if !a.gate.pass(a.picker == "loading") {
		return nil
	}
	a.picker = "error"
	a.dropCache()
	return a.failRead("/api/skills?session=" + url.QueryEscape(a.id))
}

// PickSkill takes the repo skill, the one only a catalogue read
// against the session's cwd has; a catalogue without it gets its last
// row, so a wrong list still sends something the child can judge.
func (a *skillsMentionsAdapter) PickSkill() error {
	if !a.gate.pass(a.picker == "loaded") {
		return nil
	}
	if len(a.names) == 0 {
		return fmt.Errorf("PickSkill: the picker has no rows")
	}
	pick := a.names[len(a.names)-1]
	if slices.Contains(a.names, smRepoSkill) {
		pick = smRepoSkill
	}
	a.putLead(pick)
	a.picker = "closed"
	return nil
}

// putLead is the page's pick: "/name " at the front of the draft,
// replacing a lead word only when it names a known skill, so a leading
// path stays as the rest of the message.
func (a *skillsMentionsAdapter) putLead(name string) {
	first, rest, _ := strings.Cut(a.draft, " ")
	if strings.HasPrefix(first, "/") && slices.Contains(a.names, first[1:]) {
		a.draft = rest
	}
	a.draft = "/" + name + " " + strings.TrimLeft(a.draft, " ")
	a.lead = "skill"
}

// --- the composer mentions -----------------------------------------------

func (a *skillsMentionsAdapter) mentionFree() bool {
	return a.picker == "closed" && a.mention == "none" && a.lead == "none" && !a.sent
}

// TypeSlash types "/" at the start of the draft: the one place a pick
// makes a lead skill, which is what the spec's PickMention says.
func (a *skillsMentionsAdapter) TypeSlash() error {
	if !a.gate.pass(a.mentionFree()) {
		return nil
	}
	a.draft = "/" + a.draft
	a.tokAt, a.tokEnd, a.kind = 0, 1, "/"
	a.mention = "loading"
	if a.cached {
		a.mention = "open"
	}
	return nil
}

// TypeAt types " @" at the end of the draft.
func (a *skillsMentionsAdapter) TypeAt() error {
	if !a.gate.pass(a.mentionFree()) {
		return nil
	}
	if a.draft != "" && !strings.HasSuffix(a.draft, " ") {
		a.draft += " "
	}
	a.tokAt = len(a.draft)
	a.draft += "@"
	a.tokEnd, a.kind, a.mention = len(a.draft), "@", "loading"
	return nil
}

// TypeToken types the next letter of what the walk will pick, so the
// rows it filters to always hold it.
func (a *skillsMentionsAdapter) TypeToken() error {
	if !a.gate.pass(a.mention == "open" || a.mention == "dismissed") {
		return nil
	}
	target := smRepoSkill
	if a.kind == "@" {
		target = smFile
	}
	typed := a.tokEnd - a.tokAt - 1
	letter := "z" // past the whole name: the rows go empty, as they would
	if typed < len(target) {
		letter = target[typed : typed+1]
	}
	a.draft = a.draft[:a.tokEnd] + letter + a.draft[a.tokEnd:]
	a.tokEnd++
	switch {
	case a.kind == "@", !a.cached:
		a.mention = "loading"
	default:
		a.mention = "open"
	}
	return nil
}

func (a *skillsMentionsAdapter) Dismiss() error {
	if a.gate.pass(a.mention == "loading" || a.mention == "open" || a.mention == "error") {
		a.mention = "dismissed"
	}
	return nil
}

func (a *skillsMentionsAdapter) RetryMention() error {
	if a.gate.pass(a.mention == "error") {
		a.mention = "loading"
	}
	return nil
}

func (a *skillsMentionsAdapter) token() string { return a.draft[a.tokAt+1 : a.tokEnd] }

func (a *skillsMentionsAdapter) filesPath() string {
	return "/api/files?session=" + url.QueryEscape(a.id) + "&q=" + url.QueryEscape(a.token())
}

func (a *skillsMentionsAdapter) MentionLoaded() error {
	if !a.gate.pass(a.mention == "loading") {
		return nil
	}
	if a.kind == "/" {
		if err := a.loadSkills(); err != nil {
			return err
		}
	} else {
		var body struct {
			Files []struct {
				Path string `json:"path"`
			} `json:"files"`
		}
		if err := a.get(a.filesPath(), true, &body); err != nil {
			return err
		}
		a.files = a.files[:0]
		for _, f := range body.Files {
			a.files = append(a.files, f.Path)
		}
	}
	a.mention = "open"
	return nil
}

func (a *skillsMentionsAdapter) MentionFailed() error {
	if !a.gate.pass(a.mention == "loading") {
		return nil
	}
	a.mention = "error"
	path := a.filesPath()
	if a.kind == "/" {
		a.dropCache()
		path = "/api/skills?session=" + url.QueryEscape(a.id)
	}
	return a.failRead(path)
}

// PickMention replaces the token with the row and a space. A skill pick
// takes the row the token filters to; a file pick must find the file
// the token spells among the server's rows.
func (a *skillsMentionsAdapter) PickMention() error {
	if !a.gate.pass(a.mention == "open") {
		return nil
	}
	var choice string
	if a.kind == "/" {
		for _, n := range a.names {
			if strings.HasPrefix(n, a.token()) && (choice == "" || n == smRepoSkill) {
				choice = "/" + n
			}
		}
	} else if slices.Contains(a.files, smFile) && strings.HasPrefix(smFile, a.token()) {
		choice = "@" + smFile
	}
	if choice == "" {
		return fmt.Errorf("PickMention: no row for %q%s", a.kind, a.token())
	}
	a.draft = a.draft[:a.tokAt] + choice + " " + strings.TrimLeft(a.draft[a.tokEnd:], " ")
	if a.kind == "/" {
		a.lead = "skill"
	}
	a.closeMention()
	return nil
}

func (a *skillsMentionsAdapter) closeMention() {
	a.mention, a.kind, a.tokAt, a.tokEnd = "none", "", 0, 0
}

// Paste appends text that ends in a would-be token; it opens nothing.
func (a *skillsMentionsAdapter) Paste() error {
	if a.gate.pass(a.picker == "closed") {
		a.draft += " see @foo"
		a.closeMention()
	}
	return nil
}

// --- the draft -----------------------------------------------------------

func (a *skillsMentionsAdapter) TypePath() error {
	if !a.gate.pass(a.mentionFree()) {
		return nil
	}
	a.draft = smPath + " " + a.draft
	a.lead, a.typedPath = "path", true
	return nil
}

// Send posts the draft as the page does and waits for the session to be
// done with it: a skill's turn to finish (llm-control answers it), or
// the child's reply to a "/" line that is not a command.
func (a *skillsMentionsAdapter) Send() error {
	if !a.gate.pass(a.picker == "closed" && (a.mention == "none" || a.mention == "dismissed") && a.lead != "none" && !a.sent) {
		return nil
	}
	a.turn++
	a.sends[a.lead]++
	text := strings.TrimSpace(a.draft)
	if a.lead == "skill" {
		control.Queue(a.t, a.dir, fmt.Sprintf("sm%04d", a.turn), control.Turn{Mode: "ok", Text: "ran it"})
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	a.sent, a.lead, a.typedPath, a.draft = true, "none", false, ""
	a.closeMention()
	// The child records the line, then answers it: a turn's done, or the
	// system line an unknown command gets.
	for {
		_, lines, err := a.s.GetSession(ctx, a.id)
		if err == nil && smAnswered(lines) {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("Send %q: no answer in the transcript: %v", text, ctx.Err())
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func smAnswered(lines []serve.Line) bool {
	for _, l := range lines {
		if l.Kind == "done" || l.Kind == "system" && strings.Contains(l.Text, "unknown command") {
			return true
		}
	}
	return false
}

var skillsMentionsActions = map[string]map[string]fmbt.ActionFunc{"Composer": {
	"OpenPicker":    action((*skillsMentionsAdapter).OpenPicker),
	"ClosePicker":   action((*skillsMentionsAdapter).ClosePicker),
	"RetryPicker":   action((*skillsMentionsAdapter).RetryPicker),
	"PickSkill":     action((*skillsMentionsAdapter).PickSkill),
	"TypeSlash":     action((*skillsMentionsAdapter).TypeSlash),
	"TypeAt":        action((*skillsMentionsAdapter).TypeAt),
	"TypeToken":     action((*skillsMentionsAdapter).TypeToken),
	"Dismiss":       action((*skillsMentionsAdapter).Dismiss),
	"RetryMention":  action((*skillsMentionsAdapter).RetryMention),
	"PickMention":   action((*skillsMentionsAdapter).PickMention),
	"Paste":         action((*skillsMentionsAdapter).Paste),
	"TypePath":      action((*skillsMentionsAdapter).TypePath),
	"Send":          action((*skillsMentionsAdapter).Send),
	"PickerLoaded":  action((*skillsMentionsAdapter).PickerLoaded),
	"PickerFailed":  action((*skillsMentionsAdapter).PickerFailed),
	"MentionLoaded": action((*skillsMentionsAdapter).MentionLoaded),
	"MentionFailed": action((*skillsMentionsAdapter).MentionFailed),
}}

// The runner picks each step from all 17 actions, enabled or not, and
// stops checking at the first disabled one, so a walk that opens a
// picker and loads it is about one in a hundred. Most steps are local
// and a session is reused until a Send, so walks are cheap: run many.
func skillsMentionsOptions() map[string]any {
	return map[string]any{"max-seq-runs": 5000, "max-actions": 8, "max-parallel-runs": 0}
}

// skillsMentionsHistory reads the trace off a transcript. Only Send
// leaves a record, so the steps before it are the shortest path that
// leads to the draft the child got: a lead skill came from the Skills
// button, a leading path was typed. What is checked is the Send: sent
// is whether the line reached the session as the person's (a command
// entry, which is how a "/" line lands), ran whether a skill came in
// with the input it submitted.
func skillsMentionsHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Composer#0.sent": false, "Composer#0.ran": false}}}
	var text string
	sent, ran := false, false
	for _, e := range entries {
		t, _ := e.Data["text"].(string)
		switch e.Kind {
		case "command":
			sent, text = true, t
		case "input":
			if text == "" {
				text = t
			}
			if strings.Contains(t, "\n[skill: ") {
				ran = true
			}
		}
	}
	if text == "" {
		return steps
	}
	if ran || !strings.HasPrefix(text, smPath) {
		steps = append(steps,
			tracecheck.Step{Action: "Composer#0.OpenPicker"},
			tracecheck.Step{Action: "Composer#0.PickerLoaded"},
			tracecheck.Step{Action: "Composer#0.PickSkill"})
	} else {
		steps = append(steps, tracecheck.Step{Action: "Composer#0.TypePath"})
	}
	return append(steps, tracecheck.Step{Action: "Composer#0.Send",
		State: map[string]any{"Composer#0.sent": sent, "Composer#0.ran": ran}})
}

func init() { historyProjections["skills_mentions"] = skillsMentionsHistory }

func TestSkillsMentions(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSkillsMentionsAdapter(t)
	if err := runMBT(t, "skills_mentions", a, skillsMentionsActions, skillsMentionsOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("checked steps loaded the catalogue %v and sent %v", a.loads, a.sends)
	// A skill send is four chosen steps in a row, about one walk in
	// 10^5; TestSkillsMentionsPaths walks those deliberately.
	if a.loads["loading/"] == 0 || a.loads["closed//"] == 0 {
		t.Errorf("the walks never loaded the catalogue from both pickers; raise max-seq-runs")
	}
	g, err := tracecheck.Load(fizzCheck(t, "skills_mentions"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
}

// checkHistories replays every transcript the walks wrote on g, and
// with MODEL_TRACE_DIR set keeps them there, where TestHistoryTraces
// replays them again.
func (a *skillsMentionsAdapter) checkHistories(t *testing.T, g *tracecheck.Graph) {
	t.Helper()
	keep := os.Getenv("MODEL_TRACE_DIR")
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), skillsMentionsHistory)
		if keep == "" {
			continue
		}
		b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		dir := filepath.Join(keep, "skills_mentions")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, "go-"+id+".jsonl"), b, 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

// The catalogue read without the session is the bug the flow was
// written for; a run with it must fail, or the runner is not looking.
func TestSkillsMentionsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSkillsMentionsAdapter(t)
	a.noSession = true
	if err := runMBT(t, "skills_mentions", a, skillsMentionsActions, skillsMentionsOptions()); err == nil {
		t.Fatal("a run whose catalogue ignores the session passed; the runner is not checking state")
	}
}

// The random walks rarely string four chosen steps together, and a
// skill send needs that. The generator's paths.json (the paths the
// browser spec walks) covers every transition, so this walks each of
// them against the same serve and compares every step's state. It
// reads the checked-in graph, so it needs neither fizz nor the MBT lock.
func TestSkillsMentionsPaths(t *testing.T) {
	t.Parallel()
	a := newSkillsMentionsAdapter(t)
	if err := walkSkillsMentionsPaths(t, a); err != nil {
		t.Fatal(err)
	}
	t.Logf("loaded the catalogue %v, sent %v", a.loads, a.sends)
	if a.sends["skill"] == 0 || a.sends["path"] == 0 {
		t.Errorf("the paths sent %v; want both a skill and a path lead", a.sends)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "skills_mentions"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
}

// Its wrong-adapter check: with the catalogue read ignoring the session,
// some path must disagree.
func TestSkillsMentionsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSkillsMentionsAdapter(t)
	a.noSession = true
	if walkSkillsMentionsPaths(t, a) == nil {
		t.Fatal("every path passed with a catalogue that ignores the session")
	}
}

// walkSkillsMentionsPaths returns the first step whose state is not the
// spec's, or an action that failed.
func walkSkillsMentionsPaths(t *testing.T, a *skillsMentionsAdapter) error {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "skills_mentions", "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil || len(doc.Paths) == 0 {
		t.Fatalf("paths.json: %v (%d paths)", err, len(doc.Paths))
	}
	for i, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", i, err)
		}
		for j, st := range p.Trace {
			if j > 0 {
				name := strings.TrimPrefix(st.Action, "Composer#0.")
				act, ok := skillsMentionsActions["Composer"][name]
				if !ok {
					t.Fatalf("path %d step %d: no action %q", i, j, st.Action)
				}
				if _, err := act(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", i, j, name, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d (%s): disabled in the adapter's view", i, j, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d: %w", i, j, err)
			}
			for k, want := range st.State {
				if f, ok := strings.CutPrefix(k, "Composer#0."); ok && got[f] != want {
					return fmt.Errorf("path %d step %d (%s): %s = %v, want %v", i, j, st.Action, f, got[f], want)
				}
			}
		}
	}
	return nil
}
