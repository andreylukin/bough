package wiki

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// entry is one history line for a test session.
type entry struct {
	Seq  int64          `json:"seq"`
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Data map[string]any `json:"data,omitempty"`
}

func testPaths(t *testing.T) paths {
	t.Helper()
	root := t.TempDir()
	p := paths{
		hist:  filepath.Join(root, "history"),
		wiki:  filepath.Join(root, "wiki"),
		skill: filepath.Join(root, "skills", "llm-wiki", "SKILL.md"),
		home:  root,
	}
	if err := os.MkdirAll(p.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	return p
}

// session writes a history file whose entries are at `at` and whose
// mtime is `mod`.
func session(t *testing.T, p paths, id, cwd string, at, mod time.Time, kinds ...string) {
	t.Helper()
	var b strings.Builder
	es := []entry{{Seq: 1, At: at, Kind: "meta", Data: map[string]any{"cwd": cwd}}}
	for i, k := range kinds {
		es = append(es, entry{Seq: int64(i + 2), At: at, Kind: k, Data: map[string]any{"text": k + " text"}})
	}
	for _, e := range es {
		line, _ := json.Marshal(e)
		b.Write(line)
		b.WriteByte('\n')
	}
	path := filepath.Join(p.hist, id+".jsonl")
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(path, mod, mod); err != nil {
		t.Fatal(err)
	}
}

func writeLog(t *testing.T, p paths, body string) {
	t.Helper()
	if err := os.MkdirAll(p.wiki, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(p.log(), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestFindPending(t *testing.T) {
	p := testPaths(t)
	now := time.Date(2026, 9, 11, 18, 0, 0, 0, time.UTC)
	base := now.Add(-24 * time.Hour)
	old := now.Add(-2 * time.Hour)

	session(t, p, "fresh", "/repo", now, now, "input", "assistant")                         // still in use: skipped
	session(t, p, "done", "/repo", old, old, "input", "assistant")                          // fully ingested: skipped
	session(t, p, "partial", "/repo", old, old, "input", "assistant", "input", "assistant") // ingested to #3
	session(t, p, "ingest-run", p.wiki, old, old, "input", "assistant")                     // an ingest itself: skipped
	session(t, p, "silent", "/repo", old, old, "code", "result")                            // no conversation: skipped
	session(t, p, "prehistory", "/repo", base.Add(-time.Hour), old, "input", "assistant")   // before the baseline
	session(t, p, "new", "/repo", old, old.Add(-time.Minute), "input", "assistant")

	writeLog(t, p, "# Wiki log\n\n<!-- baseline: "+base.Format(time.RFC3339)+" -->\n\n"+
		"## [2026-09-11] ingest | done#3 | No material | -\n"+
		"## [2026-09-11] ingest | partial#3 | New | topics/x/y.md\n")

	pend, err := FindPending(p, 30*time.Minute, false, now)
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]Pending{}
	var order []string
	for _, x := range pend {
		got[x.ID] = x
		order = append(order, x.ID)
	}
	if len(got) != 2 || got["partial"].From != 3 || got["partial"].To != 5 || got["new"].From != 0 {
		t.Fatalf("pending = %+v", pend)
	}
	if order[0] != "new" { // oldest first
		t.Errorf("order = %v, want new first", order)
	}

	all, err := FindPending(p, 30*time.Minute, true, now)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, x := range all {
		found = found || x.ID == "prehistory"
	}
	if !found {
		t.Errorf("--all must ignore the baseline: %+v", all)
	}
}

func TestFindPendingWithoutLog(t *testing.T) {
	p := testPaths(t)
	old := time.Now().Add(-time.Hour)
	session(t, p, "s1", "/repo", old, old, "input", "assistant")
	pend, err := FindPending(p, 30*time.Minute, false, time.Now())
	if err != nil || len(pend) != 1 || pend[0].From != 0 || pend[0].To != 3 {
		t.Fatalf("pending = %+v, %v", pend, err)
	}
}

func TestDigest(t *testing.T) {
	p := testPaths(t)
	old := time.Now().Add(-time.Hour)
	session(t, p, "s1", "/repo", old, old, "input", "assistant", "code", "result", "thinking", "sub:assistant")
	out, err := DigestFile(p, "s1", 0)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"cite as `s1#<seq>`", "#1 cwd: /repo", "#2 user: input text", "#3 assistant: assistant text", "#4 ran: code text", "#5 output: result text"} {
		if !strings.Contains(out, want) {
			t.Errorf("digest missing %q:\n%s", want, out)
		}
	}
	if strings.Contains(out, "thinking text") || strings.Contains(out, "sub:assistant") {
		t.Errorf("digest carries skipped kinds:\n%s", out)
	}
	if out, _ := DigestFile(p, "s1", 3); strings.Contains(out, "#2 ") || !strings.Contains(out, "#4 ran") {
		t.Errorf("--from 3 must start after #3:\n%s", out)
	}
	if _, err := DigestFile(p, "../etc/passwd", 0); err == nil {
		t.Error("a path as a session id must be refused")
	}

	long := strings.Repeat("line\n", 100)
	var b strings.Builder
	digestLine(&b, 9, "output", long, 10)
	if !strings.Contains(b.String(), "… 90 more lines") {
		t.Errorf("long output not cut:\n%s", b.String())
	}
}

func TestCheck(t *testing.T) {
	p := testPaths(t)
	old := time.Now().Add(-time.Hour)
	session(t, p, "s1", "/repo", old, old, "input", "assistant")
	page := func(rel, body string) {
		path := filepath.Join(p.wiki, rel)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	page("index.md", "# Wiki index\n- [Good](topics/a/good.md)\n- [Bad](topics/a/bad.md)\n")
	page("log.md", "## [2026-09-11] ingest | nosuch#9 | New | -\n") // the log is not checked for citations
	page("topics/a/good.md", "# Good\nA fact `s1#2`. See [bad](bad.md).\n")
	page("topics/a/bad.md", "# Bad\nGone `ghost#1`. Missing `s1#99`. Link [x](../b/none.md).\n")
	page("topics/a/orphan.md", "# Orphan\n")

	probs, err := Check(p)
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, pr := range probs {
		got = append(got, pr.String())
	}
	text := strings.Join(got, "\n")
	for _, want := range []string{
		"topics/a/bad.md:2: cites a session that does not exist: ghost",
		"topics/a/bad.md:2: cites entry #99, which session s1 does not have",
		"topics/a/bad.md:2: broken link ../b/none.md",
		"topics/a/orphan.md:1: not listed in index.md",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("check missing %q; got:\n%s", want, text)
		}
	}
	if len(probs) != 4 {
		t.Errorf("want exactly 4 problems, got:\n%s", text)
	}
}

// fakeBough is a stand-in for the headless ingest: it records its
// stdin, working directory and environment, and writes a page as an
// ingest would.
func fakeBough(t *testing.T, dir string) (exe, record string) {
	t.Helper()
	record = filepath.Join(dir, "record.txt")
	exe = filepath.Join(dir, "bough")
	script := "#!/bin/sh\n{ echo \"args=$*\"; echo \"pwd=$(pwd)\"; echo \"web=$BOUGH_WEB_ADDR\"; echo \"bin=$BOUGH_BIN\"; echo \"path=$PATH\"; cat; } > " + record + "\n" +
		"mkdir -p topics/t && echo '# Page' > topics/t/page.md\n"
	if err := os.WriteFile(exe, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	return exe, record
}

func TestRunIngestsPendingAndCommits(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	p := testPaths(t)
	exe, record := fakeBough(t, t.TempDir())

	// First run: creates the wiki (with a baseline of now), nothing pending.
	if err := Run(p, exe, false, 3, 30*time.Minute); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(record); err == nil {
		t.Fatal("ran an ingest with nothing pending")
	}
	if b, err := os.ReadFile(p.skill); err != nil || string(b) != skillMD {
		t.Fatalf("skill not written: %v", err)
	}
	for _, f := range []string{"index.md", "log.md", ".git"} {
		if _, err := os.Stat(filepath.Join(p.wiki, f)); err != nil {
			t.Fatalf("wiki not initialised: %v", err)
		}
	}

	// A session after the baseline, quiet for an hour: pending.
	at := time.Now().Add(time.Second)
	session(t, p, "s1", "/repo", at, at.Add(-time.Hour), "input", "assistant")
	if err := Run(p, exe, false, 3, 30*time.Minute); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(record)
	if err != nil {
		t.Fatalf("no ingest ran: %v", err)
	}
	rec := string(b)
	wikiDir, _ := filepath.EvalSymlinks(p.wiki) // /var is /private/var on macOS: pwd prints the real path
	for _, want := range []string{"args=--headless", "pwd=" + wikiDir, "web=127.0.0.1:0", "bin=" + exe, "path=" + filepath.Dir(exe) + ":", "/llm-wiki ingest s1"} {
		if !strings.Contains(rec, want) {
			t.Errorf("ingest call missing %q:\n%s", want, rec)
		}
	}
	out, _ := exec.Command("git", "-C", p.wiki, "log", "--format=%s").Output()
	if !strings.Contains(string(out), "ingest s1") || !strings.Contains(string(out), "wiki: initialise") {
		t.Errorf("commits = %q", out)
	}
}

func TestRunSkipsWhileLocked(t *testing.T) {
	p := testPaths(t)
	exe, record := fakeBough(t, t.TempDir())
	if err := ensureWiki(p); err != nil {
		t.Fatal(err)
	}
	at := time.Now().Add(time.Second)
	session(t, p, "s1", "/repo", at, at.Add(-time.Hour), "input", "assistant")
	unlock, ok := tryLock(filepath.Join(p.wiki, ".ingest.lock"))
	if !ok {
		t.Fatal("could not take the lock")
	}
	defer unlock()
	if err := Run(p, exe, false, 3, 30*time.Minute); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(record); err == nil {
		t.Fatal("an ingest ran while another held the lock")
	}
}

func TestPlist(t *testing.T) {
	p := testPaths(t)
	s := plist(p, "/usr/local/bin/bough", 5*time.Minute)
	for _, want := range []string{"<string>com.bough.wiki</string>", "<string>/usr/local/bin/bough</string>\n    <string>wiki</string>\n    <string>run</string>", "<integer>300</integer>", "<string>/usr/local/bin:/opt/homebrew/bin"} {
		if !strings.Contains(s, want) {
			t.Errorf("plist missing %q:\n%s", want, s)
		}
	}
}

func TestSkillIsManual(t *testing.T) {
	if !strings.Contains(skillMD, "\nmanual: true\n") || !strings.Contains(skillMD, "name: llm-wiki") {
		t.Fatal("the llm-wiki skill must be manual (loaded by /llm-wiki only, never on a prose mention)")
	}
}
