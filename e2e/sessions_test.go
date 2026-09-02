// Session resume suite: -c/--continue, -r/--resume, and `bough
// sessions` against the real binary. The parrot provider reports how
// many messages the model was given, which is the proof that a resumed
// session's history is projected back into the model context.
package e2e

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const parrotInit = `
bough.provider("parrot", function (system, messages) {
  var last = messages[messages.length - 1].content;
  return "parrot(" + last + ") after " + messages.length + " msgs";
});
bough.setup({ provider: { default: "parrot" } });
`

// sessionFiles globs the session dir under home.
func sessionFiles(t *testing.T, home string) []string {
	t.Helper()
	files, err := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func lineCount(t *testing.T, path string) int {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	n := 0
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		n++
	}
	return n
}

// --continue reopens the newest session file: the model sees the prior
// turn in its context, and the SAME file grows (append-only resume).
func TestHeadlessContinueRoundTrip(t *testing.T) {
	t.Parallel()
	a := launchHeadless(t, launchOpts{cwd: map[string]string{".bough/init.js": parrotInit}})
	a.send("polly")
	a.closeStdin()
	if code := a.waitExit(); code != 0 {
		t.Fatalf("run A exit %d:\n%s", code, a.out.String())
	}
	mustContain(t, a.out.String(), "parrot(polly) after 1 msgs")
	files := sessionFiles(t, a.home)
	if len(files) != 1 {
		t.Fatalf("after run A: %d session files, want 1: %v", len(files), files)
	}
	before := lineCount(t, files[0])

	b := launchHeadless(t, launchOpts{from: a, args: []string{"-c"}})
	b.send("wants a cracker")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("run B exit %d:\n%s", code, b.out.String())
	}
	// Prior input + assistant are projected: 3 messages, not 1.
	mustContain(t, b.out.String(), "parrot(wants a cracker) after 3 msgs")

	after := sessionFiles(t, b.home)
	if len(after) != 1 || after[0] != files[0] {
		t.Fatalf("continue must reuse the same file: before %v, after %v", files, after)
	}
	if got := lineCount(t, after[0]); got <= before {
		t.Fatalf("session file did not grow: %d -> %d lines", before, got)
	}
}

// --continue with no stored sessions notes it and starts fresh.
func TestHeadlessContinueNoSessions(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{args: []string{"--continue"}})
	b.send("hello")
	b.waitFor("echo: hello")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d:\n%s", code, b.out.String())
	}
	mustContain(t, b.out.String(), "no previous session, starting fresh")
}

// --resume <id> resumes that exact session, id given with or without
// the .jsonl suffix.
func TestHeadlessResumeExactID(t *testing.T) {
	t.Parallel()
	a := launchHeadless(t, launchOpts{cwd: map[string]string{".bough/init.js": parrotInit}})
	a.send("first")
	a.closeStdin()
	if code := a.waitExit(); code != 0 {
		t.Fatalf("run A exit %d:\n%s", code, a.out.String())
	}
	files := sessionFiles(t, a.home)
	if len(files) != 1 {
		t.Fatalf("want 1 session file, got %v", files)
	}
	id := strings.TrimSuffix(filepath.Base(files[0]), ".jsonl")

	b := launchHeadless(t, launchOpts{from: a, args: []string{"-r", id}})
	b.send("second")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("run B exit %d:\n%s", code, b.out.String())
	}
	mustContain(t, b.out.String(), "parrot(second) after 3 msgs")

	// The .jsonl-suffixed form resolves to the same session.
	c := launchHeadless(t, launchOpts{from: a, args: []string{"--resume", id + ".jsonl"}})
	c.send("third")
	c.closeStdin()
	if code := c.waitExit(); code != 0 {
		t.Fatalf("run C exit %d:\n%s", code, c.out.String())
	}
	mustContain(t, c.out.String(), "parrot(third) after 5 msgs")

	if after := sessionFiles(t, a.home); len(after) != 1 {
		t.Fatalf("resume must not create new files: %v", after)
	}
}

// --resume with an unknown id exits 1, naming near matches.
func TestResumeBadIDExitsOne(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{home: map[string]string{
		".bough/history/abc-session.jsonl": `{"seq":1,"kind":"input","data":{"text":"kept"}}` + "\n",
	}})
	out, code := runCLI(t, home, cwd, "--set", "llm.plugin=llm-echo", "-r", "abc", "--headless")
	if code != 1 {
		t.Fatalf("exit %d, want 1:\n%s", code, out)
	}
	mustContain(t, out, `no session "abc"`, "did you mean:", "abc-session")

	out, code = runCLI(t, home, cwd, "--set", "llm.plugin=llm-echo", "-r", "zzz", "--headless")
	if code != 1 {
		t.Fatalf("exit %d, want 1:\n%s", code, out)
	}
	mustContain(t, out, `no session "zzz"`, "bough sessions")
}

// Bare --resume in headless mode prints the session list and exits 2.
func TestBareResumeHeadlessExitsTwo(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{home: map[string]string{
		".bough/history/pick-me.jsonl": `{"seq":1,"kind":"input","data":{"text":"remember the milk"}}` + "\n",
	}})
	out, code := runCLI(t, home, cwd, "--headless", "-r")
	if code != 2 {
		t.Fatalf("exit %d, want 2:\n%s", code, out)
	}
	mustContain(t, out, "--resume needs a session id", "pick-me", "remember the milk")
}

// `bough sessions` lists sessions newest first: id, local time, entry
// count, first-input title truncated to ~60 columns.
func TestSessionsCommand(t *testing.T) {
	t.Parallel()
	longTitle := strings.Repeat("x", 80)
	home, cwd, _ := sandbox(t, launchOpts{home: map[string]string{
		".bough/history/older.jsonl": `{"seq":1,"kind":"input","data":{"text":"old question"}}` + "\n" +
			`{"seq":2,"kind":"assistant","data":{"text":"old answer"}}` + "\n",
		".bough/history/newer.jsonl": `{"seq":1,"kind":"input","data":{"text":"` + longTitle + `"}}` + "\n",
	}})
	dir := filepath.Join(home, ".bough", "history")
	base := time.Now().Add(-time.Hour)
	if err := os.Chtimes(filepath.Join(dir, "older.jsonl"), base, base); err != nil {
		t.Fatal(err)
	}

	out, code := runCLI(t, home, cwd, "sessions")
	if code != 0 {
		t.Fatalf("sessions exit %d:\n%s", code, out)
	}
	inOrder(t, out, "newer", "1 entries", "older", "2 entries", "old question")
	// The 80-rune title is truncated to ~60 columns with an ellipsis.
	mustContain(t, out, strings.Repeat("x", 59)+"…")
	mustNotContain(t, out, longTitle)

	// Empty store: a note, exit 0.
	emptyHome, emptyCwd, _ := sandbox(t, launchOpts{})
	out, code = runCLI(t, emptyHome, emptyCwd, "sessions")
	if code != 0 {
		t.Fatalf("sessions (empty) exit %d:\n%s", code, out)
	}
	mustContain(t, out, "no sessions")
}
