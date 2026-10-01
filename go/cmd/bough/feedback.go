package main

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

const feedbackRepo = "andreylukin/bough"
const feedbackTitle = "Brief issue title"
const feedbackProblem = "Describe the issue here."

func runFeedback(args []string) {
	if len(args) == 1 && (args[0] == "-h" || args[0] == "--help") {
		fmt.Fprintln(os.Stdout, "usage: bough feedback\n\nEdit and review a metadata-only report, then post it as a public GitHub issue. Requires gh auth login. Screenshots can be attached in the browser afterward.")
		return
	}
	if len(args) != 0 {
		fatal(errors.New("feedback: usage: bough feedback"))
	}
	gh, err := exec.LookPath("gh")
	if err != nil {
		fatal(errors.New("feedback: GitHub CLI (gh) is required; install it and run gh auth login"))
	}
	if err := exec.Command(gh, "auth", "status", "--hostname", "github.com").Run(); err != nil {
		fatal(errors.New("feedback: sign in to GitHub first with gh auth login"))
	}
	editor := os.Getenv("VISUAL")
	if editor == "" {
		editor = os.Getenv("EDITOR")
	}
	if editor == "" {
		editor = "vi"
	}
	if err := submitFeedback(os.Stdin, os.Stdout, os.Stderr, editor, gh, feedbackDraft(versionString(), runtime.GOOS, runtime.GOARCH, feedbackPlugins())); err != nil {
		fatal(fmt.Errorf("feedback: %w", err))
	}
}

func feedbackPlugins() string {
	rows, err := resolveConfig(false, "").load()
	if err != nil {
		return "unavailable"
	}
	var names []string
	for _, id := range []string{"llm", "loop"} {
		for _, row := range rows {
			if row.ID == id && !row.Disabled {
				names = append(names, id+"="+row.Plugin)
				break
			}
		}
	}
	if len(names) == 0 {
		return "unavailable"
	}
	return strings.Join(names, ", ")
}

func feedbackDraft(version, goos, goarch, plugins string) string {
	return fmt.Sprintf(`# %s

<!-- This becomes a public GitHub issue. Remove any work details before posting. -->

## What happened
%s

## Steps to reproduce
<!-- Add steps that do not disclose private data. -->

## Expected behavior
<!-- What should have happened? -->

## Environment
- Bough: %s
- OS/architecture: %s/%s
- Configured provider/loop: %s

<!-- Add screenshots in the browser after posting; review them first. -->
`, feedbackTitle, feedbackProblem, version, goos, goarch, plugins)
}

func parseFeedback(draft string) (title, body string, err error) {
	first, rest, ok := strings.Cut(strings.TrimSpace(draft), "\n")
	if !ok || !strings.HasPrefix(first, "# ") {
		return "", "", errors.New("draft must start with '# <issue title>'")
	}
	title = strings.TrimSpace(strings.TrimPrefix(first, "# "))
	if title == "" || title == feedbackTitle {
		return "", "", errors.New("replace the draft issue title")
	}
	body = strings.TrimSpace(rest)
	if !strings.Contains(body, "## What happened") || strings.Contains(body, feedbackProblem) {
		return "", "", errors.New("describe what happened in the draft")
	}
	return title, body + "\n", nil
}

func feedbackYes(r *bufio.Reader) bool {
	line, _ := r.ReadString('\n')
	return strings.EqualFold(strings.TrimSpace(line), "y") || strings.EqualFold(strings.TrimSpace(line), "yes")
}

func submitFeedback(in io.Reader, out, errOut io.Writer, editor, gh, initial string) error {
	dir, err := os.MkdirTemp("", "bough-feedback-")
	if err != nil {
		return err
	}
	keep := false
	defer func() {
		if !keep {
			os.RemoveAll(dir)
		}
	}()
	draft := filepath.Join(dir, "draft.md")
	if err := os.WriteFile(draft, []byte(initial), 0o600); err != nil {
		return err
	}
	edit := exec.Command("sh", "-c", editor+` "$1"`, "bough-feedback", draft)
	edit.Stdin, edit.Stdout, edit.Stderr = in, out, errOut
	if err := edit.Run(); err != nil {
		keep = true
		return fmt.Errorf("editor failed: %w; draft kept at %s", err, draft)
	}
	b, err := os.ReadFile(draft)
	if err != nil {
		return err
	}
	title, body, err := parseFeedback(string(b))
	if err != nil {
		keep = true
		return fmt.Errorf("%w; draft kept at %s", err, draft)
	}
	fmt.Fprintf(out, "\nPublic issue in %s:\n# %s\n\n%s\nPost this issue? [y/N] ", feedbackRepo, title, body)
	reader := bufio.NewReader(in)
	if !feedbackYes(reader) {
		fmt.Fprintln(out, "Not posted.")
		return nil
	}
	issue := filepath.Join(dir, "issue.md")
	if err := os.WriteFile(issue, []byte(body), 0o600); err != nil {
		return err
	}
	create := exec.Command(gh, "issue", "create", "--repo", feedbackRepo, "--title", title, "--body-file", issue)
	create.Stderr = errOut
	created, err := create.Output()
	if err != nil {
		keep = true
		return fmt.Errorf("gh issue create failed: %w; draft kept at %s", err, draft)
	}
	url := strings.TrimSpace(string(created))
	if url == "" {
		fmt.Fprintln(out, "Issue created.")
		return nil
	}
	fmt.Fprintf(out, "Issue created: %s\nOpen it in a browser to attach a screenshot? [y/N] ", url)
	if feedbackYes(reader) {
		open := exec.Command(gh, "issue", "view", url, "--web")
		open.Stdout, open.Stderr = out, errOut
		if err := open.Run(); err != nil {
			fmt.Fprintf(errOut, "Could not open browser: %v\n", err)
		}
	}
	return nil
}
