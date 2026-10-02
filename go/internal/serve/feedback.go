package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"image/png"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

const feedbackRepository = "github.com/andreylukin/bough"
const maxFeedbackImage = 10 << 20

var feedbackIssueURL = regexp.MustCompile(`^https://github\.com/andreylukin/bough/issues/[1-9][0-9]*$`)

type feedbackResult struct {
	URL       string `json:"url,omitempty"`
	Error     string `json:"error,omitempty"`
	Retryable bool   `json:"retryable"`
}

// A per-API seam keeps tests away from the host's GitHub identity.
type feedbackCommand func(context.Context, string, ...string) ([]byte, error)

// Failure output can contain remote responses or local paths. Only bounded
// stdout is needed; stderr is deliberately not sent back to the browser.
func runFeedbackGH(ctx context.Context, dir string, args ...string) ([]byte, error) {
	gh, err := exec.LookPath("gh")
	if err != nil {
		return nil, err
	}
	return feedbackExec(ctx, gh, dir, args...)
}

func feedbackExec(ctx context.Context, gh, dir string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, gh, args...)
	cmd.WaitDelay = 5 * time.Second
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), "GH_HOST=github.com", "GH_PROMPT_DISABLED=1", "GH_PAGER=cat", "GH_DEBUG=", "NO_COLOR=1")
	var out feedbackOutput
	cmd.Stdout = &out
	err := cmd.Run()
	return out.data.Bytes(), err
}

type feedbackOutput struct{ data bytes.Buffer }

func (b *feedbackOutput) Write(p []byte) (int, error) {
	n := len(p)
	if left := (64 << 10) - b.data.Len(); left > 0 {
		if len(p) > left {
			p = p[:left]
		}
		_, _ = b.data.Write(p)
	}
	return n, nil
}

// Only an explicit multipart submit uploads anything. The normal serve Guard
// supplies token, origin and host checks; no GitHub credential enters the UI.
func (a *API) feedback(w http.ResponseWriter, r *http.Request) {
	if !a.feedbackBusy.CompareAndSwap(false, true) {
		writeJSON(w, http.StatusConflict, feedbackResult{Error: "Another feedback submission is still running. Wait for its result.", Retryable: true})
		return
	}
	defer a.feedbackBusy.Store(false)
	fail := func(code int, message string) { writeJSON(w, code, feedbackResult{Error: message, Retryable: true}) }
	r.Body = http.MaxBytesReader(w, r.Body, maxFeedbackImage+(64<<10))
	if err := r.ParseMultipartForm(maxFeedbackImage + (64 << 10)); err != nil {
		if r.MultipartForm != nil {
			_ = r.MultipartForm.RemoveAll()
		}
		fail(http.StatusBadRequest, "Use one PNG screenshot of 10 MB or smaller and a shorter report.")
		return
	}
	defer r.MultipartForm.RemoveAll()
	title, details := strings.TrimSpace(r.FormValue("title")), strings.TrimSpace(r.FormValue("details"))
	if title == "" || len(title) > 256 || strings.ContainsAny(title, "\r\n") || details == "" || len(details) > 32000 || r.FormValue("share") != "public" {
		fail(http.StatusBadRequest, "Provide a title (up to 256 bytes), details (up to 32000 bytes), and consent to publish the report and screenshot.")
		return
	}
	files := r.MultipartForm.File["screenshot"]
	if len(files) != 1 || len(r.MultipartForm.File) != 1 || files[0].Size <= 0 || files[0].Size > maxFeedbackImage {
		fail(http.StatusBadRequest, "Choose one PNG screenshot of 10 MB or smaller.")
		return
	}
	file, err := files[0].Open()
	if err != nil {
		fail(http.StatusBadRequest, "Could not read the screenshot.")
		return
	}
	data, err := io.ReadAll(io.LimitReader(file, maxFeedbackImage+1))
	_ = file.Close()
	if err != nil || len(data) > maxFeedbackImage {
		fail(http.StatusBadRequest, "Could not read the screenshot within the 10 MB limit.")
		return
	}
	cfg, err := png.DecodeConfig(bytes.NewReader(data))
	if err != nil || cfg.Width <= 0 || cfg.Height <= 0 || cfg.Width > 8192 || cfg.Height > 8192 || int64(cfg.Width)*int64(cfg.Height) > 16_000_000 {
		fail(http.StatusBadRequest, "Use a valid PNG screenshot up to 8192 pixels per side and 16 million pixels.")
		return
	}
	if _, err := png.Decode(bytes.NewReader(data)); err != nil {
		fail(http.StatusBadRequest, "Could not decode the PNG screenshot.")
		return
	}
	dir, err := os.MkdirTemp("", "bough-feedback-")
	if err != nil {
		fail(http.StatusInternalServerError, "Could not prepare feedback files on the Bough server.")
		return
	}
	defer os.RemoveAll(dir)
	run := a.feedbackGH
	if run == nil {
		run = runFeedbackGH
	}
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Minute)
	defer cancel()
	help, err := run(ctx, dir, "issue", "create", "--help")
	if errors.Is(err, exec.ErrNotFound) {
		fail(http.StatusServiceUnavailable, "Install GitHub CLI (gh) on the machine running Bough to submit screenshots.")
		return
	}
	if err != nil || !strings.Contains(string(help), "--attach") {
		fail(http.StatusServiceUnavailable, "Update GitHub CLI on the machine running Bough to a version supporting gh issue create --attach.")
		return
	}
	if _, err := run(ctx, dir, "auth", "status", "--hostname", "github.com"); err != nil {
		fail(http.StatusServiceUnavailable, "GitHub authentication is unavailable. Check gh auth status --hostname github.com on the machine running Bough.")
		return
	}
	permissions, err := run(ctx, dir, "repo", "view", feedbackRepository, "--json", "viewerPermission")
	var repo struct {
		ViewerPermission string `json:"viewerPermission"`
	}
	if err != nil || json.Unmarshal(permissions, &repo) != nil {
		fail(http.StatusServiceUnavailable, "Could not check GitHub repository access. Run gh repo view github.com/andreylukin/bough --json viewerPermission on the machine running Bough; resolve any access restriction before retrying.")
		return
	}
	switch repo.ViewerPermission {
	case "WRITE", "MAINTAIN", "ADMIN":
	default:
		fail(http.StatusForbidden, "Automatic screenshot attachments require GitHub push access to andreylukin/bough. Download the screenshot and attach it on GitHub instead.")
		return
	}
	body := "## What happened\n" + details + "\n\n## Environment\nbough web\n"
	if err := os.WriteFile(filepath.Join(dir, "report.md"), []byte(body), 0o600); err != nil {
		fail(http.StatusInternalServerError, "Could not prepare the report.")
		return
	}
	if err := os.WriteFile(filepath.Join(dir, "screenshot.png"), data, 0o600); err != nil {
		fail(http.StatusInternalServerError, "Could not prepare the screenshot.")
		return
	}
	// Exactly one attachment: gh refuses to create an issue when that upload
	// fails. Never retry a create: a lost response can hide a successful issue.
	out, err := run(ctx, dir, "issue", "create", "--repo", feedbackRepository, "--title", title, "--body-file", "report.md", "--attach", "screenshot.png")
	issue := strings.TrimSpace(string(out))
	result := feedbackResult{}
	if feedbackIssueURL.MatchString(issue) {
		result.URL = issue
	}
	if err != nil || result.URL == "" {
		result.Error = "GitHub submission could not be confirmed. Check andreylukin/bough issues before submitting again; the screenshot may already be uploaded. Check gh authentication supports attachments and repository push access."
		writeJSON(w, http.StatusBadGateway, result)
		return
	}
	writeJSON(w, http.StatusCreated, result)
}
