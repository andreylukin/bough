package wiki

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"time"
)

// Run is one scheduler tick: take the lock, find what is pending, and
// if anything is, run a headless bough in the wiki directory with
// "/llm-wiki ingest <ids>" and commit what it wrote. With nothing
// pending it returns at once and never calls a model, so a 5-minute
// schedule costs nothing on a quiet day. only, when set, ingests that
// one pending session and nothing else (the control room's "Ingest").
func Run(p paths, exe string, all bool, maxSessions int, quiet time.Duration, only string) error {
	if err := ensureWiki(p); err != nil {
		return err
	}
	unlock, ok := tryLock(filepath.Join(p.wiki, ".ingest.lock"))
	if !ok {
		return nil // the previous tick's ingest is still running
	}
	defer unlock()
	if err := writeSkill(p); err != nil {
		return err
	}
	// The brief rides the same tick: due or not is decided here, so a
	// quiet day still costs nothing (briefDue is three stats).
	var briefErr error
	if due, _ := briefDue(p, time.Now(), false); due && only == "" {
		briefErr = runBrief(p, exe)
	}
	pend, err := FindPending(p, quiet, all, time.Now())
	if only != "" {
		pend = slices.DeleteFunc(pend, func(x Pending) bool { return x.ID != only })
	}
	if err != nil || len(pend) == 0 {
		return errors.Join(err, briefErr)
	}
	if len(pend) > maxSessions {
		pend = pend[:maxSessions]
	}
	var ids []string
	for _, x := range pend {
		ids = append(ids, x.ID)
	}
	fmt.Printf("%s ingest %s\n", time.Now().Format(time.RFC3339), strings.Join(ids, " "))
	ctx, cancel := context.WithTimeout(context.Background(), runTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, exe, "--headless")
	cmd.Dir = p.wiki // sessions started here are the ingests: FindPending skips them
	cmd.Stdin = strings.NewReader("/llm-wiki ingest " + strings.Join(ids, " ") + "\n")
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	cmd.Env = append(os.Environ(),
		// Never take the user's artifact server port.
		"BOUGH_WEB_ADDR=127.0.0.1:0",
		// The skill shells out to "${BOUGH_BIN:-bough}" wiki …: this
		// binary, whatever the shell's PATH resolves `bough` to (a login
		// shell puts an older install first).
		"BOUGH_BIN="+exe,
		// The ingest writes wiki pages: without this it runs as a plain
		// local session, read-only, and refused every run.
		"BOUGH_WRITE_ROOTS="+p.wiki,
		"PATH="+filepath.Dir(exe)+string(os.PathListSeparator)+os.Getenv("PATH"),
	)
	runErr := cmd.Run()
	commit(p, "ingest "+strings.Join(ids, " "))
	return errors.Join(runErr, briefErr)
}

// ensureWiki creates the wiki directory, its index and log (the log
// with the baseline that keeps the first run from ingesting all of
// history), and a git repo so every ingest is one reviewable commit.
func ensureWiki(p paths) error { return ensureWikiChecked(p, func() {}) }

// ensureWikiChecked is ensureWiki with its check-then-create boundary
// exposed as a hook: it stats every component up front, calls
// atCheckpoint, then creates whatever was missing and commits iff it
// created something. Production always passes a no-op; EnsureWikiForTest
// is the only caller that uses the hook, to drive the install-vs-first-run
// race in go/tests/model/mbt/wiki_install_first_run_vs_serve_tick_test.go
// deterministically instead of relying on real thread-scheduling luck.
func ensureWikiChecked(p paths, atCheckpoint func()) error {
	if err := os.MkdirAll(p.wiki, 0o755); err != nil {
		return err
	}
	logMissing := statMissing(p.log())
	indexMissing := statMissing(p.index())
	ignore := filepath.Join(p.wiki, ".gitignore")
	ignoreMissing := statMissing(ignore)
	gitDir := filepath.Join(p.wiki, ".git")
	_, hasGit := exec.LookPath("git")
	gitMissing := hasGit == nil && statMissing(gitDir)

	atCheckpoint()

	created := false
	if logMissing {
		log := fmt.Sprintf("# Wiki log\n\n<!-- baseline: %s -->\n\nAppend-only. One `## [YYYY-MM-DD] ingest | <session>#<seq> | <disposition> | <pages>` heading per ingested session; `bough wiki pending` reads them.\n", time.Now().UTC().Format(time.RFC3339))
		if err := os.WriteFile(p.log(), []byte(log), 0o644); err != nil {
			return err
		}
		created = true
	}
	if indexMissing {
		if err := os.WriteFile(p.index(), []byte("# Wiki index\n\nOne line per page, grouped by `## <topic>` headings (format: see the llm-wiki skill).\n"), 0o644); err != nil {
			return err
		}
		created = true
	}
	if ignoreMissing {
		_ = os.WriteFile(ignore, []byte(".ingest.lock\ningest.log\n"), 0o644)
	}
	if gitMissing {
		_ = git(p.wiki, "init", "-q")
		created = true
	}
	if created {
		commit(p, "wiki: initialise")
	}
	return nil
}

func statMissing(path string) bool {
	_, err := os.Stat(path)
	return errors.Is(err, fs.ErrNotExist)
}

// EnsureWikiForTest runs ensureWiki against wikiDir with atCheckpoint
// called between its check and its create-if-missing, so a test can pause
// two real callers there and force a specific interleaving. It exists for
// go/tests/model/mbt/wiki_install_first_run_vs_serve_tick_test.go, the
// only place two callers race this function on purpose.
func EnsureWikiForTest(wikiDir string, atCheckpoint func()) error {
	return ensureWikiChecked(paths{wiki: wikiDir}, atCheckpoint)
}

// commit records whatever the ingest changed; a no-op when git is
// missing or nothing changed.
func commit(p paths, msg string) {
	if _, err := os.Stat(filepath.Join(p.wiki, ".git")); err != nil {
		return
	}
	_ = git(p.wiki, "add", "-A")
	if git(p.wiki, "diff", "--cached", "--quiet") == nil {
		return // nothing staged
	}
	_ = git(p.wiki, "-c", "user.name=bough", "-c", "user.email=bough@localhost", "commit", "-q", "-m", msg)
}

// commitPage records one hand edit (a Save, a review decision) as its
// page alone. An ingest or brief may be running with its agent halfway
// through other files, and `add -A` here swept those into "edit P",
// leaving the run's own commit without them.
func commitPage(p paths, msg, rel string) {
	if _, err := os.Stat(filepath.Join(p.wiki, ".git")); err != nil {
		return
	}
	_ = git(p.wiki, "add", "--", rel)
	if git(p.wiki, "diff", "--cached", "--quiet", "--", rel) == nil {
		return // nothing staged
	}
	_ = git(p.wiki, "-c", "user.name=bough", "-c", "user.email=bough@localhost", "commit", "-q", "-m", msg, "--only", "--", rel)
}

func git(dir string, args ...string) error {
	cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
	var out bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &out
	return cmd.Run()
}

// writeSkill puts the embedded llm-wiki skill where the skills row
// finds it, when it is missing or differs from this binary's.
func writeSkill(p paths) error {
	if b, err := os.ReadFile(p.skill); err == nil && string(b) == skillMD {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(p.skill), 0o755); err != nil {
		return err
	}
	return os.WriteFile(p.skill, []byte(skillMD), 0o644)
}
