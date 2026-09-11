package vtreal

// Surface "headless-reads-stdin-large-prompt-unicode": a 2MB prompt
// line carrying invalid UTF-8 and NUL bytes piped into `bough
// --headless`. The binary must not panic, must exit 0, and must store
// the prompt in history as valid JSON whose text is valid UTF-8 — the
// raw bytes, or a deterministic U+FFFD replacement. The history input
// entry is the text the loop hands the model seam (plugins/replay's
// Model sees it as the user message), so asserting it is asserting
// the seam.

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

// headlessReadsStdinLargePromptUnicodeFixture is the deterministic
// 2MB prompt: an ASCII head, then a repeating block of valid
// multi-byte runes, NULs, lone continuation bytes, a truncated
// sequence and 0xff, with no newline anywhere.
func headlessReadsStdinLargePromptUnicodeFixture() []byte {
	block := []byte("héllo 世界 🌲\x00\xff\x80abc\xc3\x00\xe2\x82 ")
	var b bytes.Buffer
	b.WriteString("summarize this blob: ")
	for b.Len() < 2<<20 {
		b.Write(block)
	}
	return b.Bytes()
}

func headlessReadsStdinLargePromptUnicodeRun(t *testing.T, stdin []byte) (headlessResult, string) {
	t.Helper()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", "headless_answer.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, "-config", cfg, "--headless")
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_HEADLESS_IDLE=30",
	)
	cmd.Stdin = bytes.NewReader(stdin)
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	runErr := cmd.Run()
	res := headlessResult{stdout: out.String(), stderr: errb.String()}
	if ee, ok := runErr.(*exec.ExitError); ok {
		res.code = ee.ExitCode()
	} else if runErr != nil {
		t.Fatalf("running bough --headless: %v\n%.4000s", runErr, res.screen())
	}
	if ctx.Err() != nil {
		t.Fatalf("bough --headless never exited:\n%.4000s", res.screen())
	}
	return res, home
}

// headlessReadsStdinLargePromptUnicodeInputs returns the text of every
// input entry in the history files under home, failing on any line
// that is not valid JSON.
func headlessReadsStdinLargePromptUnicodeInputs(t *testing.T, home string) []string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if len(paths) == 0 {
		t.Fatalf("no history file under %s", home)
	}
	var inputs []string
	for _, p := range paths {
		f, err := os.Open(p)
		if err != nil {
			t.Fatal(err)
		}
		sc := bufio.NewScanner(f)
		sc.Buffer(nil, 64<<20)
		for sc.Scan() {
			var e struct {
				Kind string `json:"kind"`
				Data struct {
					Text string `json:"text"`
				} `json:"data"`
			}
			if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
				t.Errorf("%s: history line is not JSON: %v (%.200q)", p, err, sc.Bytes())
				continue
			}
			if e.Kind == "input" {
				inputs = append(inputs, e.Data.Text)
			}
		}
		if err := sc.Err(); err != nil {
			t.Errorf("%s: %v", p, err)
		}
		f.Close()
	}
	return inputs
}

func TestHeadlessReadsStdinLargePromptUnicode(t *testing.T) {
	t.Parallel()
	fx := headlessReadsStdinLargePromptUnicodeFixture()

	t.Run("FixtureDeterministic", func(t *testing.T) {
		a := sha256.Sum256(fx)
		b := sha256.Sum256(headlessReadsStdinLargePromptUnicodeFixture())
		if a != b || len(fx) < 2<<20 || utf8.Valid(fx) || bytes.IndexByte(fx, 0) < 0 || bytes.IndexByte(fx, '\n') >= 0 {
			t.Fatalf("fixture is not a deterministic 2MB invalid-UTF-8 NUL line: len %d sha %s", len(fx), hex.EncodeToString(a[:]))
		}
	})

	t.Run("NoPanicExitZeroHistoryUTF8", func(t *testing.T) {
		t.Parallel()
		r, home := headlessReadsStdinLargePromptUnicodeRun(t, append(append([]byte(nil), fx...), '\n'))
		if strings.Contains(r.stderr, "panic:") || strings.Contains(r.stderr, "goroutine ") {
			t.Fatalf("bough panicked:\n%.4000s", r.screen())
		}
		if r.code != 0 {
			t.Errorf("want exit 0:\n%.4000s", r.screen())
		}
		if !utf8.ValidString(r.stdout) {
			t.Errorf("stdout is not valid UTF-8")
		}
		sawAnswer := false
		for _, ev := range headlessEvents(r.stdout) {
			if ev[0] == "assistant" && ev[1] == "Two Go files: a.go and b.go." {
				sawAnswer = true
			}
		}
		if !sawAnswer {
			t.Errorf("turn never reached the tape's answer:\n%.4000s", r.screen())
		}
		inputs := headlessReadsStdinLargePromptUnicodeInputs(t, home)
		if len(inputs) != 1 {
			t.Fatalf("want one input entry, got %d", len(inputs))
		}
		got := inputs[0]
		if !utf8.ValidString(got) {
			t.Errorf("stored prompt (the model seam's user message) is not valid UTF-8")
		}
		// Deterministic replacement: one U+FFFD per invalid byte (what
		// encoding/json does) or per invalid run (strings.ToValidUTF8).
		var perByte strings.Builder
		for s := string(fx); s != ""; {
			r, n := utf8.DecodeRuneInString(s)
			perByte.WriteRune(r)
			s = s[n:]
		}
		if got != perByte.String() && got != strings.ToValidUTF8(string(fx), "�") {
			t.Errorf("stored prompt is neither deterministically replaced form: len %d vs fixture %d, head %.120q", len(got), len(fx), got)
		}
		if !strings.Contains(got, "\x00") {
			t.Errorf("NUL bytes were dropped from the stored prompt")
		}
	})
}
