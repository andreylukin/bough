// Package prwatch is the "pr-watch" plugin: it keeps your open pull
// requests moving while you work on something else. Every interval it
// lists your open PRs in the session's repo, reads their review
// threads, conversation comments and CI, and when something needs a
// hand it runs a subagent in its own git worktree to deal with it:
//
//   - a review thread from a bot (Codex, Devin, Copilot, …): reply, fix
//     it on the branch when it is right, and resolve the thread once the
//     fix is pushed; reply and leave it open when it is wrong;
//   - a review thread from a person: the same, but never resolve it —
//     people close their own threads;
//   - a conversation comment that asks something: answer it;
//   - CI failed or is stuck: read the failed logs, fix, push.
//
// Every session runs the watcher, wherever it was started: PRs are
// found with gh search, not from the session's directory. A shared
// state file under ~/.bough/prwatch keeps sessions from working the
// same PR twice and lets each one show, under its composer, which PRs
// are being worked and by whom. Nothing about a job reaches the
// transcript: its steps go to a per-job log that /background prints. The subagent never touches your
// checkouts: the watcher keeps its own clones under ~/.bough/prwatch/
// repos and gives each job a detached worktree of the PR's head, which
// pushes to the branch by name.
package prwatch

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

const (
	defaultInterval = 5 * time.Minute
	defaultDays     = 14
	defaultMaxPRs   = 3
	// lockStale is how long a session may hold a PR before another
	// session treats it as abandoned (a crashed session, a dead laptop).
	lockStale = 45 * time.Minute
	// showDone is how long a finished job stays in the strip.
	showDone = 20 * time.Minute
)

// defaultBots are the review bots whose threads the subagent may resolve
// once the fix is pushed. Any login ending in "[bot]" counts too.
var defaultBots = []string{"codex", "devin", "copilot", "coderabbit", "cursor", "sourcery", "graphite", "greptile"}

// Config is the row's config.
type Config struct {
	Interval time.Duration
	Days     int
	MaxPRs   int
	Authors  []string
	Bots     []string
	Me       string // override for the gh login (tests, or an org account)
}

// Watcher is the plugin's state for one session.
type Watcher struct {
	cfg     Config
	home    string // ~/.bough/prwatch
	me      string
	session string
	run     runner
	spawn   func(ctx context.Context, task string, shape map[string]any, sink func(kind, text string)) (any, error)
	ctx     context.Context
	state   *stateFile
	// worktreeFn makes the job's checkout; tests replace it.
	worktreeFn func(ctx context.Context, pr PR) (string, func(), error)
	// cloneMu serialises clone/fetch per repo within the session.
	cloneMu sync.Mutex

	mu      sync.Mutex
	failed  bool
	logs    map[string]*jobLog // this session's jobs, by PR key
	errs    []string           // recent errors, newest last
	active  int                // PRs being worked by any session, refreshed every few seconds
	working []Working          // the same PRs, for the attention board
	recent  map[string]Recent  // last finished job per PR, for the board's hover
}

// Recent is the last pr-watch job on a PR.
type Recent struct {
	Key     string // owner/name#n
	Summary string
	At      time.Time
}

// Recent is the last finished job for a PR key (owner/name#n or
// name#n), if any. Cached like Active.
func (w *Watcher) Recent(key string) (Recent, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if r, ok := w.recent[key]; ok {
		return r, true
	}
	for k, r := range w.recent {
		if _, short, _ := strings.Cut(k, "/"); short == key {
			return r, true
		}
	}
	return Recent{}, false
}

// Working is one PR a session is working right now.
type Working struct {
	Key     string // owner/name#n
	Session string
	Since   time.Time
	What    string
}

// Working lists the PRs being worked by any session, oldest first.
// Cached like Active.
func (w *Watcher) Working() []Working {
	w.mu.Lock()
	defer w.mu.Unlock()
	return slices.Clone(w.working)
}

// Active is the status bar's number: PRs being worked right now by any
// session. Cached; the shared file is read on a timer, not per frame.
func (w *Watcher) Active() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.active
}

func (w *Watcher) refreshActive() {
	st, err := w.state.load()
	if err != nil {
		return
	}
	var live []Working
	recent := map[string]Recent{}
	for key, ps := range st.PRs {
		if ps.Lock != nil && time.Since(ps.Lock.Since) < lockStale {
			live = append(live, Working{Key: key, Session: ps.Lock.Session, Since: ps.Lock.Since, What: ps.Lock.What})
		}
		if ps.Last != "" {
			recent[key] = Recent{Key: key, Summary: ps.Last, At: ps.LastAt}
		}
	}
	slices.SortFunc(live, func(a, b Working) int { return a.Since.Compare(b.Since) })
	w.mu.Lock()
	w.active = len(live)
	w.working = live
	w.recent = recent
	w.mu.Unlock()
}

// jobLog is one job's steps, kept for /background; nothing about a
// background job reaches the transcript.
type jobLog struct {
	Key   string
	Since time.Time
	Done  bool
	Lines []string
}

const jobLogMax = 200

func (w *Watcher) logLine(key, line string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.logs == nil {
		w.logs = map[string]*jobLog{}
	}
	l := w.logs[key]
	if l == nil {
		l = &jobLog{Key: key, Since: time.Now()}
		w.logs[key] = l
	}
	l.Lines = append(l.Lines, line)
	if len(l.Lines) > jobLogMax {
		l.Lines = l.Lines[len(l.Lines)-jobLogMax:]
	}
}

// ---------- state shared across sessions ----------

// prState is what the watcher remembers about one PR across polls and
// sessions.
type prState struct {
	Seen     []string       `json:"seen,omitempty"`     // comment ids already answered or judged
	Attempts map[string]int `json:"attempts,omitempty"` // jobs started per comment id, until seen
	Blocked  string         `json:"blocked,omitempty"`  // why the last job could not act; cleared by a new head or comment
	CIHead   string         `json:"ci_head,omitempty"`  // head sha whose CI was last attempted
	Lock     *lock          `json:"lock,omitempty"`     // who is working it now
	Last     string         `json:"last,omitempty"`     // the last job's summary
	LastAt   time.Time      `json:"last_at,omitzero"`
	Title    string         `json:"title,omitempty"`
	URL      string         `json:"url,omitempty"`
}

type lock struct {
	Session string    `json:"session"`
	Since   time.Time `json:"since"`
	What    string    `json:"what"`
}

type state struct {
	PRs map[string]*prState `json:"prs"`
}

// stateFile is ~/.bough/prwatch/<repo>.json, read fresh and written
// whole under a process-local mutex; cross-session races are bounded
// by the lock's session field and the stale timeout.
type stateFile struct {
	path string
	mu   sync.Mutex
}

func (f *stateFile) load() (*state, error) {
	st := &state{PRs: map[string]*prState{}}
	b, err := os.ReadFile(f.path)
	if errors.Is(err, os.ErrNotExist) {
		return st, nil
	}
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(b, st); err != nil {
		return nil, fmt.Errorf("prwatch: state %s: %w", f.path, err)
	}
	if st.PRs == nil {
		st.PRs = map[string]*prState{}
	}
	return st, nil
}

func (f *stateFile) save(st *state) error {
	if err := os.MkdirAll(filepath.Dir(f.path), 0o755); err != nil {
		return err
	}
	b, _ := json.MarshalIndent(st, "", " ")
	tmp := f.path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, f.path)
}

// update applies fn under the file mutex and writes the result.
func (f *stateFile) update(fn func(*state) error) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	st, err := f.load()
	if err != nil {
		return err
	}
	if err := fn(st); err != nil {
		return err
	}
	return f.save(st)
}

// ---------- policy ----------

// isBot reports whether a login is any bot: the "[bot]" suffix GitHub
// gives apps, or a name from bots.
func isBot(login string, bots []string) bool {
	return strings.HasSuffix(strings.ToLower(login), "[bot]") || namedBot(login, bots)
}

// namedBot reports whether a login is one of the review bots by name:
// the ones whose conversation comments are worth answering, unlike a
// dependabot status note.
func namedBot(login string, bots []string) bool {
	l := strings.ToLower(login)
	for _, b := range bots {
		if strings.Contains(l, strings.ToLower(b)) {
			return true
		}
	}
	return false
}

// Work is what one PR needs, as the watcher judged it.
type Work struct {
	Threads  []Thread  // unresolved threads whose last word is not mine and not yet handled
	Comments []Comment // conversation comments not mine and not yet handled
	CI       []Check   // failed checks on the current head, not yet attempted
	CIStuck  bool      // no check finished and the head is old enough that it should have
}

// ids is every comment id in the work: threads' last comments and
// conversation comments.
func (w Work) ids() []string {
	var out []string
	for _, t := range w.Threads {
		out = append(out, t.Comments[len(t.Comments)-1].ID)
	}
	for _, c := range w.Comments {
		out = append(out, c.ID)
	}
	return out
}

// Empty reports nothing to do.
func (w Work) Empty() bool {
	return len(w.Threads) == 0 && len(w.Comments) == 0 && len(w.CI) == 0 && !w.CIStuck
}

// maxAttempts is how many jobs may start on one comment before the
// watcher stops asking.
const maxAttempts = 2

// judge decides what a PR needs given what was already handled.
func judge(pr PR, me string, bots []string, ps *prState, now time.Time) Work {
	seen := map[string]bool{}
	for _, id := range ps.Seen {
		seen[id] = true
	}
	// A comment two jobs have started on and not marked is one the
	// child cannot deal with: asking a third time would only spend.
	for id, n := range ps.Attempts {
		if n >= maxAttempts {
			seen[id] = true
		}
	}
	var w Work
	for _, t := range pr.Threads {
		if t.Resolved || len(t.Comments) == 0 {
			continue
		}
		last := t.Comments[len(t.Comments)-1]
		if strings.EqualFold(last.Author, me) || seen[last.ID] {
			continue
		}
		w.Threads = append(w.Threads, t)
	}
	for _, c := range pr.Comments {
		if strings.EqualFold(c.Author, me) || seen[c.ID] {
			continue
		}
		// A person's comment, or a review bot's. Other bots (dependabot,
		// CI summaries) post status, not questions.
		if !isBot(c.Author, nil) || namedBot(c.Author, bots) {
			w.Comments = append(w.Comments, c)
		}
	}
	if ps.CIHead != pr.HeadSHA {
		anyDone := false
		for _, c := range pr.Checks {
			if c.Failed() {
				w.CI = append(w.CI, c)
			}
			if !c.Pending() {
				anyDone = true
			}
		}
		if len(pr.Checks) > 0 && !anyDone && now.Sub(pr.Updated) > 30*time.Minute {
			w.CIStuck = true
		}
	}
	return w
}

// ---------- the job ----------

// reportShape is what the subagent must hand back, so the watcher can
// mark what was handled without parsing prose.
var reportShape = map[string]any{
	"type": "object",
	"properties": map[string]any{
		"handled":  map[string]any{"type": "array", "items": map[string]any{"type": "string"}, "description": "comment ids (review comment database ids or conversation comment node ids) that were replied to"},
		"noted":    map[string]any{"type": "array", "items": map[string]any{"type": "string"}, "description": "comment ids you read and judged to need no reply (informational, already addressed, a bot's status)"},
		"blocked":  map[string]any{"type": "string", "description": "when you could not act on the CI failure or a comment for a reason outside this PR (depends on another PR, no access, needs the author): one sentence; empty otherwise"},
		"resolved": map[string]any{"type": "array", "items": map[string]any{"type": "string"}, "description": "review thread ids that were resolved"},
		"pushed":   map[string]any{"type": "boolean", "description": "whether a commit was pushed to the PR branch"},
		"summary":  map[string]any{"type": "string", "description": "one or two sentences for the human"},
	},
	"required": []any{"handled", "noted", "resolved", "pushed", "summary"},
}

// task writes the subagent's brief: the facts the watcher saw, the
// rules, and the exact commands, so the child spends its steps on the
// fix rather than on discovering GitHub.
func (w *Watcher) task(pr PR, work Work, wt string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "You are handling pull request #%d %q (%s) on branch %s in repository %s/%s, on behalf of %s.\n\n", pr.Number, pr.Title, pr.URL, pr.Branch, pr.Owner, pr.Name, w.me)
	fmt.Fprintf(&b, "WORKTREE: %s is a detached checkout of the PR head (%s). Every tools.bash command that touches the code MUST start with `cd %s &&`. Never touch any other directory. To publish a fix: commit there and run `git push origin HEAD:%s`.\n\n", wt, pr.HeadSHA[:min(12, len(pr.HeadSHA))], wt, pr.Branch)
	b.WriteString("RULES:\n")
	b.WriteString("- Read each item, decide whether it is right, and act. Keep replies short and specific; sign nothing.\n")
	b.WriteString("- A thread from a REVIEW BOT: if the point is valid, fix it in the worktree, run the relevant tests, commit, push, reply naming the commit, then RESOLVE the thread. If it is not valid, reply why and leave it open.\n")
	b.WriteString("- A thread from a PERSON: the same, but NEVER resolve it. People close their own threads.\n")
	b.WriteString("- A conversation comment: answer it if it asks something; otherwise leave it.\n")
	b.WriteString("- CI: read the failed logs and fix what THIS PR broke, then push. A failure that also happens on the base branch, or on one runner only for reasons unrelated to the diff, is not yours to fix: name it in the summary and move on. If CI is stuck (nothing ran), say so in the summary; do not re-trigger it.\n")
	b.WriteString("- One fix commit per item is fine; combine trivially related ones. Do not rewrite history, do not force-push, do not touch unrelated code.\n")
	b.WriteString("- Run every command in the foreground: never pass a time limit to tools.bash (a background job would wake the user's own session). Run only the tests near your change, not the whole suite.\n\n")
	b.WriteString("COMMANDS (use exactly these; all go through tools.bash):\n")
	fmt.Fprintf(&b, "- Reply to a review thread: gh api repos/%s/%s/pulls/%d/comments/<COMMENT_ID>/replies -f body='...'\n", pr.Owner, pr.Name, pr.Number)
	b.WriteString("- Resolve a review thread (bots only): gh api graphql -f query='mutation { resolveReviewThread(input:{threadId:\"<THREAD_ID>\"}) { thread { isResolved } } }'\n")
	fmt.Fprintf(&b, "- Reply in the conversation: gh api repos/%s/%s/issues/%d/comments -f body='...'\n", pr.Owner, pr.Name, pr.Number)
	fmt.Fprintf(&b, "- CI: gh pr checks %d --repo %s/%s ; failed logs: gh run view <RUN_ID> --repo %s/%s --log-failed (the run id is in the check link)\n\n", pr.Number, pr.Owner, pr.Name, pr.Owner, pr.Name)
	if len(work.Threads) > 0 {
		b.WriteString("REVIEW THREADS needing a reply:\n")
		for _, t := range work.Threads {
			kind := "person"
			if isBot(t.Comments[len(t.Comments)-1].Author, w.cfg.Bots) {
				kind = "REVIEW BOT"
			}
			fmt.Fprintf(&b, "\n[thread %s] %s:%d (%s)\n", t.ID, t.Path, t.Line, kind)
			for _, c := range t.Comments {
				fmt.Fprintf(&b, "  comment %s by %s at %s:\n    %s\n", c.ID, c.Author, c.At.Format("Jan 2 15:04"), strings.ReplaceAll(strings.TrimSpace(c.Body), "\n", "\n    "))
			}
		}
		b.WriteString("\n")
	}
	if len(work.Comments) > 0 {
		b.WriteString("CONVERSATION COMMENTS:\n")
		for _, c := range work.Comments {
			fmt.Fprintf(&b, "\n[comment %s] by %s at %s:\n  %s\n", c.ID, c.Author, c.At.Format("Jan 2 15:04"), strings.ReplaceAll(strings.TrimSpace(c.Body), "\n", "\n  "))
		}
		b.WriteString("\n")
	}
	if len(work.CI) > 0 {
		b.WriteString("CI FAILURES on the current head:\n")
		for _, c := range work.CI {
			fmt.Fprintf(&b, "- %s: %s %s\n", c.Name, c.State, c.Link)
		}
		b.WriteString("\n")
	}
	if work.CIStuck {
		b.WriteString("CI appears STUCK: checks exist but none has finished in over 30 minutes.\n\n")
	}
	b.WriteString("When done, report the JSON object described by the schema: handled (comment ids you replied to), noted (comment ids you read and decided need no reply), resolved (thread ids you resolved), pushed, blocked (one sentence when something outside this PR stops you: another PR it depends on, missing access, a question only the author can answer), summary. Every comment id in the brief belongs in handled or noted; the watcher asks again about any it does not see.")
	return b.String()
}

// repoDir is the watcher's own clone of owner/name, made on first use
// and fetched per job. Never the user's checkout.
func (w *Watcher) repoDir(ctx context.Context, pr PR) (string, error) {
	w.cloneMu.Lock()
	defer w.cloneMu.Unlock()
	dir := filepath.Join(w.home, "repos", pr.Owner+"_"+pr.Name)
	if _, err := os.Stat(filepath.Join(dir, ".git")); err != nil {
		if err := os.MkdirAll(filepath.Dir(dir), 0o755); err != nil {
			return "", err
		}
		if out, err := runCmd(ctx, w.home, "gh", "repo", "clone", pr.Owner+"/"+pr.Name, dir, "--", "--quiet"); err != nil {
			return "", fmt.Errorf("gh repo clone %s/%s: %w: %s", pr.Owner, pr.Name, err, firstLine(out))
		}
	}
	return dir, nil
}

// worktree makes a detached checkout of the PR head from the watcher's
// clone, fresh each job, and returns its path and a remover.
func (w *Watcher) worktree(ctx context.Context, pr PR) (string, func(), error) {
	repo, err := w.repoDir(ctx, pr)
	if err != nil {
		return "", nil, err
	}
	path := filepath.Join(w.home, "wt", pr.Owner+"_"+pr.Name, fmt.Sprintf("pr-%d-%s", pr.Number, w.session[:min(8, len(w.session))]))
	git := func(args ...string) error {
		out, err := runCmd(ctx, repo, "git", args...)
		if err != nil {
			return fmt.Errorf("git %s: %w: %s", args[0], err, firstLine(out))
		}
		return nil
	}
	_ = git("worktree", "remove", "--force", path) // a leftover from a crashed job
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return "", nil, err
	}
	if err := git("fetch", "--quiet", "origin", pr.Branch); err != nil {
		return "", nil, err
	}
	if err := git("worktree", "add", "--detach", path, pr.HeadSHA); err != nil {
		return "", nil, err
	}
	return path, func() { _ = git("worktree", "remove", "--force", path) }, nil
}

// handle runs one PR's job: lock, worktree, subagent, record, unlock.
func prKey(pr PR) string { return fmt.Sprintf("%s/%s#%d", pr.Owner, pr.Name, pr.Number) }

func (w *Watcher) handle(pr PR, work Work) {
	key := prKey(pr)
	what := describe(work)
	locked := false
	err := w.state.update(func(st *state) error {
		ps := st.PRs[key]
		if ps == nil {
			ps = &prState{}
			st.PRs[key] = ps
		}
		if ps.Lock != nil && time.Since(ps.Lock.Since) < lockStale && ps.Lock.Session != w.session {
			return nil // another session has it
		}
		ps.Lock = &lock{Session: w.session, Since: time.Now(), What: what}
		ps.Title, ps.URL = pr.Title, pr.URL
		locked = true
		return nil
	})
	if err != nil {
		w.reportOnce(err)
		return
	}
	if !locked {
		return
	}
	w.refreshActive()
	defer func() {
		_ = w.state.update(func(st *state) error {
			if ps := st.PRs[key]; ps != nil && ps.Lock != nil && ps.Lock.Session == w.session {
				ps.Lock = nil
			}
			return nil
		})
		w.refreshActive()
	}()
	w.logLine(key, fmt.Sprintf("start · %s — %s", pr.Title, what))
	mk := w.worktreeFn
	if mk == nil {
		mk = w.worktree
	}
	wt, remove, err := mk(w.ctx, pr)
	if err != nil {
		w.reportOnce(err)
		return
	}
	defer remove()
	_ = w.state.update(func(st *state) error {
		ps := st.PRs[key]
		if ps == nil {
			ps = &prState{}
			st.PRs[key] = ps
		}
		if ps.Attempts == nil {
			ps.Attempts = map[string]int{}
		}
		for _, id := range work.ids() {
			ps.Attempts[id]++
		}
		return nil
	})
	v, err := w.spawn(w.ctx, w.task(pr, work, wt), reportShape, func(kind, text string) {
		if kind == "start" {
			return // the brief; the start line above says what the job is
		}
		w.logLine(key, kind+" · "+oneLine(strings.TrimSpace(text), 160))
	})
	if err != nil {
		w.logLine(key, "failed · "+firstLine(err.Error()))
		w.reportOnce(err)
		return
	}
	rep, _ := v.(map[string]any)
	summary, _ := rep["summary"].(string)
	handled := strs(rep["handled"])
	noted := strs(rep["noted"])
	blocked, _ := rep["blocked"].(string)
	pushed, _ := rep["pushed"].(bool)
	if len(rep) == 0 {
		// The child did not follow the schema: keep its prose, mark
		// nothing handled, so the next poll asks again.
		summary = fmt.Sprint(v)
		if len(summary) > 200 {
			summary = summary[:200] + "…"
		}
	}
	_ = w.state.update(func(st *state) error {
		ps := st.PRs[key]
		if ps == nil {
			ps = &prState{}
			st.PRs[key] = ps
		}
		// Everything the child replied to is done; what it did not
		// answer comes back next poll. CI counts as attempted for this
		// head whether or not the fix worked: a second try on the same
		// head would only repeat itself.
		done := slices.Concat(handled, noted)
		for _, id := range work.ids() {
			if slices.Contains(done, id) {
				ps.Seen = append(ps.Seen, id)
				delete(ps.Attempts, id)
			}
		}
		ps.Blocked = strings.TrimSpace(blocked)
		if len(work.CI) > 0 || work.CIStuck {
			ps.CIHead = pr.HeadSHA
		}
		ps.Seen = slices.Compact(slices.Sorted(slices.Values(ps.Seen)))
		ps.Last = summary
		ps.LastAt = time.Now()
		return nil
	})
	tail := ""
	if pushed {
		tail = " (pushed)"
	}
	w.logLine(key, "done"+tail+" · "+summary)
	w.mu.Lock()
	if l := w.logs[key]; l != nil {
		l.Done = true
	}
	w.mu.Unlock()
}

func strs(v any) []string {
	var out []string
	if l, ok := v.([]any); ok {
		for _, x := range l {
			if s, ok := x.(string); ok {
				out = append(out, s)
			}
		}
	}
	return out
}

// describe is the strip's one-line reason for a job.
func describe(w Work) string {
	var parts []string
	if n := len(w.Threads); n > 0 {
		parts = append(parts, fmt.Sprintf("%d review thread%s", n, plural(n)))
	}
	if n := len(w.Comments); n > 0 {
		parts = append(parts, fmt.Sprintf("%d comment%s", n, plural(n)))
	}
	if n := len(w.CI); n > 0 {
		parts = append(parts, fmt.Sprintf("%d failed check%s", n, plural(n)))
	}
	if w.CIStuck {
		parts = append(parts, "CI stuck")
	}
	return strings.Join(parts, ", ")
}

func plural(n int) string {
	if n == 1 {
		return ""
	}
	return "s"
}

// ---------- the poll ----------

// poll is one pass over the repo's PRs.
func (w *Watcher) poll() {
	ctx, cancel := context.WithTimeout(w.ctx, 2*time.Minute)
	defer cancel()
	prs, err := listPRs(ctx, w.run, w.home, w.cfg.Authors, time.Now().Add(-time.Duration(w.cfg.Days)*24*time.Hour), 20)
	if err != nil {
		w.reportOnce(err)
		return
	}
	if len(prs) > w.cfg.MaxPRs {
		prs = prs[:w.cfg.MaxPRs]
	}
	st, err := w.state.load()
	if err != nil {
		w.reportOnce(err)
		return
	}
	for i := range prs {
		pr := &prs[i]
		if err := fill(ctx, w.run, w.home, pr); err != nil {
			w.reportOnce(err)
			continue
		}
		ps := st.PRs[prKey(*pr)]
		if ps == nil {
			ps = &prState{}
		}
		if ps.Lock != nil && time.Since(ps.Lock.Since) < lockStale {
			continue
		}
		work := judge(*pr, w.me, w.cfg.Bots, ps, time.Now())
		if work.Empty() {
			continue
		}
		w.handle(*pr, work) // one PR at a time per session
	}
}

func (w *Watcher) loop() {
	w.poll()
	t := time.NewTicker(w.cfg.Interval)
	defer t.Stop()
	for {
		select {
		case <-w.ctx.Done():
			return
		case <-t.C:
			w.poll()
		}
	}
}

// watchActive keeps the status bar's count current across sessions.
func (w *Watcher) watchActive() {
	w.refreshActive()
	for {
		select {
		case <-w.ctx.Done():
			return
		case <-time.After(5 * time.Second):
			w.refreshActive()
		}
	}
}

// reportOnce records an error for /background and the verbose log; a
// background watcher never writes to the transcript.
func (w *Watcher) reportOnce(err error) {
	if errors.Is(err, context.Canceled) || strings.Contains(err.Error(), "signal: killed") {
		return // the row was remounted or the session ended mid-call
	}
	w.mu.Lock()
	w.failed = true
	w.errs = append(w.errs, time.Now().Format("15:04")+" "+firstLine(err.Error()))
	if len(w.errs) > 10 {
		w.errs = w.errs[len(w.errs)-10:]
	}
	w.mu.Unlock()
	kernel.Logf("pr-watch: %v\n", err)
}

// Background is what /background prints: the strip's rows, this
// session's job logs, and recent errors.
func (w *Watcher) Background() string {
	var b strings.Builder
	rows := w.Rows()
	if len(rows) == 0 {
		fmt.Fprintf(&b, "pr-watch: nothing in progress; next poll within %s\n", w.cfg.Interval)
	}
	for _, r := range rows {
		b.WriteString(r + "\n")
	}
	w.mu.Lock()
	keys := slices.Sorted(maps.Keys(w.logs))
	for _, k := range keys {
		l := w.logs[k]
		state := "running"
		if l.Done {
			state = "finished"
		}
		fmt.Fprintf(&b, "\n%s · %s · started %s ago\n", k, state, shortDur(l.Since))
		lines := l.Lines
		if len(lines) > 30 {
			lines = lines[len(lines)-30:]
		}
		for _, ln := range lines {
			b.WriteString("  " + ln + "\n")
		}
	}
	if len(w.errs) > 0 {
		b.WriteString("\nerrors:\n")
		for _, e := range w.errs {
			b.WriteString("  " + e + "\n")
		}
	}
	w.mu.Unlock()
	return strings.TrimRight(b.String(), "\n")
}

// Rows is the strip under the composer: every PR any session is
// working, and recent results, from the shared file.
func (w *Watcher) Rows() []string {
	st, err := w.state.load()
	if err != nil {
		return nil
	}
	var rows []string
	for key, ps := range st.PRs {
		if ps.Lock != nil && time.Since(ps.Lock.Since) < lockStale {
			who := "this session"
			if ps.Lock.Session != w.session {
				who = "session " + ps.Lock.Session[:min(8, len(ps.Lock.Session))]
			}
			rows = append(rows, fmt.Sprintf("pr %s %s · %s · %s · %s", key, oneLine(ps.Title, 40), ps.Lock.What, who, shortDur(ps.Lock.Since)))
		} else if ps.Blocked != "" {
			rows = append(rows, fmt.Sprintf("pr %s blocked · %s", key, oneLine(ps.Blocked, 90)))
		} else if !ps.LastAt.IsZero() && time.Since(ps.LastAt) < showDone {
			rows = append(rows, fmt.Sprintf("pr %s done %s ago · %s", key, shortDur(ps.LastAt), oneLine(ps.Last, 70)))
		}
	}
	slices.Sort(rows)
	return rows
}

func oneLine(s string, n int) string {
	s = firstLine(s)
	if r := []rune(s); len(r) > n {
		return string(r[:n]) + "…"
	}
	return s
}

func shortDur(since time.Time) string {
	d := time.Since(since).Round(time.Second)
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	}
	return fmt.Sprintf("%dh%02dm", int(d.Hours()), int(d.Minutes())%60)
}

// releaseMine drops the locks this session holds.
func (w *Watcher) releaseMine() {
	_ = w.state.update(func(st *state) error {
		for _, ps := range st.PRs {
			if ps.Lock != nil && ps.Lock.Session == w.session {
				ps.Lock = nil
			}
		}
		return nil
	})
}

// pruneWorktrees removes worktrees no live lock accounts for: a job
// killed by a restart leaves its checkout behind, and seven of them
// were found for one PR.
func (w *Watcher) pruneWorktrees() {
	st, err := w.state.load()
	if err != nil {
		return
	}
	live := map[string]bool{}
	for key, ps := range st.PRs {
		if ps.Lock != nil && time.Since(ps.Lock.Since) < lockStale {
			// wt/<owner>_<name>/pr-<n>-<session8>
			repo, n, _ := strings.Cut(key, "#")
			live[strings.ReplaceAll(repo, "/", "_")+"/pr-"+n+"-"+ps.Lock.Session[:min(8, len(ps.Lock.Session))]] = true
		}
	}
	root := filepath.Join(w.home, "wt")
	repos, _ := os.ReadDir(root)
	for _, r := range repos {
		if !r.IsDir() {
			continue
		}
		wts, _ := os.ReadDir(filepath.Join(root, r.Name()))
		clone := filepath.Join(w.home, "repos", r.Name())
		for _, d := range wts {
			if !d.IsDir() || live[r.Name()+"/"+d.Name()] {
				continue
			}
			path := filepath.Join(root, r.Name(), d.Name())
			if _, err := runCmd(w.ctx, clone, "git", "worktree", "remove", "--force", path); err != nil {
				_ = os.RemoveAll(path)
			}
		}
		_, _ = runCmd(w.ctx, clone, "git", "worktree", "prune")
	}
}

// ---------- plugin ----------

type plugin struct{}

func init() {
	kernel.Register("pr-watch", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "pr-watch" }
func (plugin) Inject() []string { return []string{"spawn-background", "history"} }

// History is the seam for the session id.
type History interface{ Path() string }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	c := Config{Interval: defaultInterval, Days: defaultDays, MaxPRs: defaultMaxPRs, Authors: []string{"@me"}, Bots: defaultBots}
	for k, v := range cfg {
		switch k {
		case "interval_minutes":
			n, err := toInt(v)
			if err != nil || n < 1 {
				return fmt.Errorf("pr-watch: interval_minutes must be a positive integer, got %v", v)
			}
			c.Interval = time.Duration(n) * time.Minute
		case "days":
			n, err := toInt(v)
			if err != nil || n < 1 {
				return fmt.Errorf("pr-watch: days must be a positive integer, got %v", v)
			}
			c.Days = n
		case "max_prs":
			n, err := toInt(v)
			if err != nil || n < 1 {
				return fmt.Errorf("pr-watch: max_prs must be a positive integer, got %v", v)
			}
			c.MaxPRs = n
		case "authors":
			if l := strList(v); len(l) > 0 {
				c.Authors = l
			}
		case "bots":
			if l := strList(v); len(l) > 0 {
				c.Bots = l
			}
		case "me":
			c.Me, _ = v.(string)
		default:
			return fmt.Errorf("pr-watch: unknown config key %q", k)
		}
	}
	spawn, err := kernel.Get[func(context.Context, string, map[string]any, func(string, string)) (any, error)](kctx, "spawn-background")
	if err != nil {
		return fmt.Errorf("pr-watch: needs the workers row (spawn-background)")
	}
	h, err := kernel.Get[History](kctx, "history")
	if err != nil {
		return fmt.Errorf("pr-watch: needs the history service")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	root := filepath.Join(home, ".bough", "prwatch")
	w := &Watcher{
		cfg: c, home: root,
		session: strings.TrimSuffix(filepath.Base(h.Path()), filepath.Ext(h.Path())),
		run:     ghRunner, spawn: spawn,
		state: &stateFile{path: filepath.Join(root, "state.json")},
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(context.Background())
	w.ctx = ctx
	kctx.Effect(func() {
		// A restart kills the job: give the PR back rather than
		// leave it locked for 45 minutes, and drop the half-edited
		// worktree.
		cancel()
		w.releaseMine()
	})
	go w.pruneWorktrees()
	kctx.Provide("pr-watch", w)
	go w.watchActive()
	if reg, err := kernel.Get[*commands.Registry](kctx, "commands"); err == nil {
		show := func(string) (string, error) { return w.Background(), nil }
		for _, name := range []string{"background", "prs"} {
			info := commands.CommandInfo{Name: name, Summary: "what pr-watch is doing in the background: PRs being worked, this session's job steps, errors"}
			if err := reg.Register(info, show); err == nil {
				kctx.Effect(func() { reg.Unregister(name) })
			}
		}
	}
	go func() {
		// Identity and repo are read once; a gh that is not logged in
		// reports once and the watcher stays quiet.
		ictx, icancel := context.WithTimeout(ctx, 30*time.Second)
		defer icancel()
		me := c.Me
		if me == "" {
			var err error
			if me, err = whoami(ictx, w.run, root); err != nil {
				w.reportOnce(err)
				return
			}
		}
		w.me = me
		w.loop()
	}()
	return nil
}

func strList(v any) []string {
	var out []string
	switch l := v.(type) {
	case []string:
		out = l
	case []any:
		for _, x := range l {
			if s, ok := x.(string); ok {
				out = append(out, s)
			}
		}
	case string:
		for _, s := range strings.Split(l, ",") {
			if s = strings.TrimSpace(s); s != "" {
				out = append(out, s)
			}
		}
	}
	return out
}

func toInt(v any) (int, error) {
	switch n := v.(type) {
	case int:
		return n, nil
	case int64:
		return int(n), nil
	case float64:
		return int(n), nil
	case string:
		return strconv.Atoi(n)
	}
	return 0, fmt.Errorf("not an integer: %v", v)
}
