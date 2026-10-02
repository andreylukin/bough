package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"image"
	"image/png"
	"io"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func feedbackPNG(t *testing.T) []byte {
	t.Helper()
	var b bytes.Buffer
	if err := png.Encode(&b, image.NewNRGBA(image.Rect(0, 0, 2, 2))); err != nil {
		t.Fatal(err)
	}
	return b.Bytes()
}

func feedbackRequest(t *testing.T, data []byte, share string) *http.Request {
	t.Helper()
	var body bytes.Buffer
	m := multipart.NewWriter(&body)
	_ = m.WriteField("title", `A screenshot $(not-a-command)`)
	_ = m.WriteField("details", "Reviewed details & punctuation")
	_ = m.WriteField("share", share)
	f, err := m.CreateFormFile("screenshot", "../../private.png")
	if err != nil {
		t.Fatal(err)
	}
	_, _ = f.Write(data)
	_ = m.Close()
	r := httptest.NewRequest("POST", "http://localhost/api/feedback", &body)
	r.Header.Set("Content-Type", m.FormDataContentType())
	return r
}

func TestFeedbackUploadsReviewedScreenshot(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	data := feedbackPNG(t)
	var commands [][]string
	var directory string
	a := NewAPI(nil)
	a.home = home
	a.feedbackGH = func(ctx context.Context, dir string, args ...string) ([]byte, error) {
		directory = dir
		commands = append(commands, args)
		if _, ok := ctx.Deadline(); !ok {
			t.Fatal("gh must be bounded")
		}
		switch len(commands) {
		case 1:
			return []byte("--attach file"), nil
		case 2:
			return nil, nil
		case 3:
			return []byte(`{"viewerPermission":"WRITE"}`), nil
		case 4:
			want := []string{"issue", "create", "--repo", feedbackRepository, "--title", `A screenshot $(not-a-command)`, "--body-file", "report.md", "--attach", "screenshot.png"}
			if !reflect.DeepEqual(args, want) {
				t.Fatalf("args %q", args)
			}
			for name, want := range map[string][]byte{"screenshot.png": data, "report.md": []byte("## What happened\nReviewed details & punctuation\n\n## Environment\nbough web\n")} {
				p := filepath.Join(dir, name)
				got, err := os.ReadFile(p)
				if err != nil || !bytes.Equal(got, want) {
					t.Fatalf("%s: %q %v", name, got, err)
				}
				st, _ := os.Stat(p)
				if st.Mode().Perm()&0077 != 0 {
					t.Fatal("public temporary file")
				}
			}
			return []byte("https://github.com/andreylukin/bough/issues/123\n"), nil
		}
		t.Fatalf("unexpected command %q", args)
		return nil, nil
	}
	w := httptest.NewRecorder()
	a.ServeHTTP(w, feedbackRequest(t, data, "public"))
	if w.Code != http.StatusCreated || !strings.Contains(w.Body.String(), `"url":"https://github.com/andreylukin/bough/issues/123"`) {
		t.Fatalf("%d %s", w.Code, w.Body.String())
	}
	if len(commands) != 4 || !reflect.DeepEqual(commands[1], []string{"auth", "status", "--hostname", "github.com"}) || !reflect.DeepEqual(commands[2], []string{"repo", "view", feedbackRepository, "--json", "viewerPermission"}) {
		t.Fatalf("commands %q", commands)
	}
	if _, err := os.Stat(directory); !os.IsNotExist(err) {
		t.Fatalf("temporary report retained: %v", err)
	}
}

func TestFeedbackFailuresDoNotCreateTextOnlyIssues(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		stage int
		out   string
		err   error
		want  string
		retry bool
	}{
		{"missing", 1, "", exec.ErrNotFound, "Install GitHub CLI", true},
		{"old", 1, "--body-file", nil, "Update GitHub CLI", true},
		{"auth", 2, "secret must not leak", errors.New("secret"), "GitHub authentication", true},
		{"denied", 3, "", errors.New("Forbidden"), "resolve any access restriction", true},
		{"read only", 3, `{"viewerPermission":"READ"}`, nil, "push access", true},
		{"unknown permission", 3, `{}`, nil, "push access", true},
		{"upload failure", 4, "", errors.New("upload failed"), "could not be confirmed", false},
		{"ambiguous create", 4, "", nil, "could not be confirmed", false},
		{"invalid URL", 4, "https://evil.example/issues/123", nil, "could not be confirmed", false},
		{"partial result", 4, "https://github.com/andreylukin/bough/issues/123", errors.New("failed"), "could not be confirmed", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			a := NewAPI(nil)
			a.home = t.TempDir()
			calls := 0
			dir := ""
			a.feedbackGH = func(_ context.Context, d string, _ ...string) ([]byte, error) {
				calls++
				dir = d
				if calls == tc.stage {
					return []byte(tc.out), tc.err
				}
				if calls > tc.stage {
					t.Fatal("submission continued after failure")
				}
				if calls == 1 {
					return []byte("--attach"), nil
				}
				if calls == 3 {
					return []byte(`{"viewerPermission":"ADMIN"}`), nil
				}
				return nil, nil
			}
			w := httptest.NewRecorder()
			a.ServeHTTP(w, feedbackRequest(t, feedbackPNG(t), "public"))
			retry := `"retryable":false`
			if tc.retry {
				retry = `"retryable":true`
			}
			if w.Code < 400 || !strings.Contains(w.Body.String(), tc.want) || !strings.Contains(w.Body.String(), retry) || strings.Contains(w.Body.String(), "secret") {
				t.Fatalf("%d %s", w.Code, w.Body.String())
			}
			if calls != tc.stage {
				t.Fatalf("calls %d", calls)
			}
			if _, err := os.Stat(dir); !os.IsNotExist(err) {
				t.Fatalf("temporary directory retained: %v", err)
			}
		})
	}
}

func TestFeedbackRejectsUnreviewedAndInvalidImages(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, share string
		data        []byte
	}{
		{"no consent", "", feedbackPNG(t)}, {"empty", "public", nil}, {"fake PNG", "public", []byte("not PNG")},
		{"oversized", "public", make([]byte, maxFeedbackImage+1)}, {"truncated", "public", feedbackPNG(t)[:40]},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			a := NewAPI(nil)
			a.home = t.TempDir()
			a.feedbackGH = func(context.Context, string, ...string) ([]byte, error) {
				t.Fatal("gh called for invalid input")
				return nil, nil
			}
			w := httptest.NewRecorder()
			a.ServeHTTP(w, feedbackRequest(t, tc.data, tc.share))
			if w.Code != http.StatusBadRequest {
				t.Fatalf("%d %s", w.Code, w.Body.String())
			}
		})
	}
}

func TestFeedbackUsesServeGuard(t *testing.T) {
	t.Parallel()
	a := NewAPI(nil)
	a.home = t.TempDir()
	a.feedbackGH = func(context.Context, string, ...string) ([]byte, error) {
		t.Fatal("unauthorized gh call")
		return nil, nil
	}
	for _, origin := range []string{"", "https://evil.example"} {
		r := feedbackRequest(t, feedbackPNG(t), "public")
		r.Header.Set("Origin", origin)
		if origin != "" {
			r.Header.Set("Authorization", "Bearer test-token")
		}
		w := httptest.NewRecorder()
		Guard(a, "test-token", false, "").ServeHTTP(w, r)
		if w.Code != http.StatusUnauthorized && w.Code != http.StatusForbidden {
			t.Fatalf("%d %s", w.Code, w.Body.String())
		}
	}
}

func TestFeedbackBoundsCommandOutput(t *testing.T) {
	t.Parallel()
	var b feedbackOutput
	if _, err := io.Copy(&b, strings.NewReader(strings.Repeat("x", 1<<20))); err != nil {
		t.Fatal(err)
	}
	if b.data.Len() != 64<<10 {
		t.Fatalf("unbounded command output: %d", b.data.Len())
	}
}

func TestFeedbackCommandProcess(t *testing.T) {
	t.Parallel()
	if len(os.Args) > 1 && os.Args[len(os.Args)-1] == "feedback-process-helper" {
		dir, _ := os.Getwd()
		_ = json.NewEncoder(os.Stdout).Encode(map[string]string{"dir": dir, "arg": os.Args[len(os.Args)-2], "host": os.Getenv("GH_HOST"), "prompt": os.Getenv("GH_PROMPT_DISABLED")})
		_, _ = os.Stderr.WriteString("private failure text")
		os.Exit(0)
	}
	bin, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(t.TempDir(), "private")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	arg := `$(touch should-not-exist); --repo other/repo`
	out, err := feedbackExec(context.Background(), bin, dir, "-test.run=^TestFeedbackCommandProcess$", "--", arg, "feedback-process-helper")
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]string
	if err := json.Unmarshal(out, &got); err != nil {
		t.Fatalf("%q %v", out, err)
	}
	canonicalDir, _ := filepath.EvalSymlinks(dir)
	canonicalGot, _ := filepath.EvalSymlinks(got["dir"])
	if canonicalGot != canonicalDir || got["arg"] != arg || got["host"] != "github.com" || got["prompt"] != "1" {
		t.Fatalf("process: %v", got)
	}
	if strings.Contains(string(out), "private failure text") {
		t.Fatal("stderr leaked")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := feedbackExec(ctx, bin, dir, "-test.run=^TestFeedbackCommandProcess$"); err == nil {
		t.Fatal("cancellation ignored")
	}
}

func TestFeedbackSerializesUploads(t *testing.T) {
	t.Parallel()
	a := NewAPI(nil)
	a.home = t.TempDir()
	entered, release := make(chan struct{}), make(chan struct{})
	a.feedbackGH = func(context.Context, string, ...string) ([]byte, error) {
		close(entered)
		<-release
		return nil, exec.ErrNotFound
	}
	first := httptest.NewRecorder()
	done := make(chan struct{})
	r := feedbackRequest(t, feedbackPNG(t), "public")
	go func() { defer close(done); a.ServeHTTP(first, r) }()
	<-entered
	w := httptest.NewRecorder()
	a.ServeHTTP(w, feedbackRequest(t, feedbackPNG(t), "public"))
	close(release)
	<-done
	if w.Code != http.StatusConflict {
		t.Fatalf("concurrent upload: %d", w.Code)
	}
	if a.feedbackBusy.Load() {
		t.Fatal("failed upload kept the lock")
	}
}
