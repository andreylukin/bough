package vtreal

// Voice-mode keys without audio: /voice toggles the mic chip, Space is
// push-to-talk only while it is on, and with no recorder on PATH /voice
// refuses instead of spawning one. The llm row is llm-openai (the only
// llm.Transcriber) pointed at a dead port with a fake key; no test
// submits a turn, so it is never called. The "recorder" is a shell
// script named rec that logs its start to a marker file and idles
// until SIGINT, so a take is observable and always "heard nothing".

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const voiceKeysConfig = `
- id: llm
  plugin: llm-openai
  config: {model: gpt-test, base_url: "http://127.0.0.1:1"}
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: auto-memory
  plugin: auto-memory
  disabled: true
- id: memory-tier
  plugin: memory-tier
  disabled: true
- id: activity
  plugin: activity
  disabled: true
- id: attention
  plugin: attention
  disabled: true
`

const voiceKeysChip = "🎤 space"

// voiceKeysStart boots bough like startCfg, but with PATH set to a
// fresh directory holding only a fake rec when withRec. It returns the
// app and the marker file the fake rec appends to on every start.
func voiceKeysStart(t *testing.T, withRec bool) (*app, string) {
	t.Helper()
	home := t.TempDir()
	pathDir := filepath.Join(home, "path")
	marker := filepath.Join(home, "rec-started")
	if err := os.Mkdir(pathDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if withRec {
		script := fmt.Sprintf("#!/bin/sh\necho started >> %q\ntrap 'exit 0' INT TERM\nwhile :; do /bin/sleep 0.05; done\n", marker)
		if err := os.WriteFile(filepath.Join(pathDir, "rec"), []byte(script), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(voiceKeysConfig), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	// Later duplicates win in exec's env, so these override the host's.
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "PATH="+pathDir, "OPENAI_API_KEY=sk-test",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a, marker
}

func (a *app) voiceKeysSlash(line string) {
	a.t.Helper()
	a.typeText(line)
	a.waitFor(line)
	a.key(uv.KeyEnter, 0)
}

// voiceKeysStarts counts the fake recorder's launches.
func voiceKeysStarts(marker string) int {
	b, _ := os.ReadFile(marker)
	return strings.Count(string(b), "started")
}

func (a *app) voiceKeysComposer() string {
	a.t.Helper()
	lines := a.lines()
	i := composerRow(lines)
	if i < 0 {
		a.t.Fatalf("composer not on screen\nscreen:\n%s", a.text())
	}
	return lines[i]
}

func (a *app) voiceKeysComposerHas(sub, what string) {
	a.t.Helper()
	a.waitUntil(func(string) bool { return strings.Contains(a.voiceKeysComposer(), sub) }, what)
}

func TestVoiceKeysNoTranscriber(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30) // llm-echo has no audio endpoint
	a.voiceKeysSlash("/voice hold")
	a.waitFor("no transcription endpoint")
	if s := a.settled(); strings.Contains(s, voiceKeysChip) {
		t.Fatalf("mic chip shown though /voice was refused\nscreen:\n%s", s)
	}
}

func TestVoiceKeysNoRecorderOnPath(t *testing.T) {
	t.Parallel()
	a, marker := voiceKeysStart(t, false)
	a.voiceKeysSlash("/voice hold")
	a.waitFor("no recorder found")
	if s := a.settled(); strings.Contains(s, voiceKeysChip) {
		t.Fatalf("mic chip shown with no recorder on PATH\nscreen:\n%s", s)
	}
	// Space stays an ordinary key: nothing to record with.
	a.typeText("   x")
	a.voiceKeysComposerHas("   x", "held spaces typed into the composer")
	if n := voiceKeysStarts(marker); n != 0 {
		t.Fatalf("recorder started %d times with none on PATH\nscreen:\n%s", n, a.text())
	}
}

func TestVoiceKeysChipToggles(t *testing.T) {
	t.Parallel()
	a, marker := voiceKeysStart(t, true)
	if s := a.settled(); strings.Contains(s, voiceKeysChip) {
		t.Fatalf("mic chip shown before /voice\nscreen:\n%s", s)
	}
	a.voiceKeysSlash("/voice hold")
	a.waitFor("voice on (hold)")
	a.waitFor(voiceKeysChip)
	a.voiceKeysSlash("/voice off")
	a.waitFor("voice off")
	a.waitUntil(func(s string) bool { return !strings.Contains(s, voiceKeysChip) }, "mic chip to clear after /voice off")
	a.voiceKeysSlash("/voice tap")
	a.waitFor("voice on (tap)")
	a.waitFor(voiceKeysChip)
	a.voiceKeysSlash("/voice") // bare /voice toggles off
	a.waitUntil(func(s string) bool { return !strings.Contains(s, voiceKeysChip) }, "mic chip to clear after bare /voice")
	if n := voiceKeysStarts(marker); n != 0 {
		t.Fatalf("toggling alone started the recorder %d times\nscreen:\n%s", n, a.text())
	}
}

func TestVoiceKeysSpaceWhenOff(t *testing.T) {
	t.Parallel()
	a, marker := voiceKeysStart(t, true)
	a.typeText("a   b") // a burst of spaces with voice off is just spaces
	a.voiceKeysComposerHas("a   b", "spaces typed with voice off")
	time.Sleep(500 * time.Millisecond)
	if n := voiceKeysStarts(marker); n != 0 {
		t.Fatalf("recorder started %d times with voice off\nscreen:\n%s", n, a.text())
	}
}

func TestVoiceKeysTapSpaceRecords(t *testing.T) {
	t.Parallel()
	a, marker := voiceKeysStart(t, true)
	a.voiceKeysSlash("/voice tap")
	a.waitFor(voiceKeysChip)
	// While composing, a space is a space.
	a.typeText("hi there")
	a.voiceKeysComposerHas("hi there", "typed space in tap mode")
	if n := voiceKeysStarts(marker); n != 0 {
		t.Fatalf("space inside a draft started the recorder\nscreen:\n%s", a.text())
	}
	for range len("hi there") {
		a.key(uv.KeyBackspace, 0)
	}
	a.voiceKeysComposerHas("say something", "composer to empty (placeholder back)")
	a.key(uv.KeySpace, 0)
	a.waitFor("space to stop")
	a.waitUntil(func(string) bool { return voiceKeysStarts(marker) == 1 }, "fake rec to start once")
	a.key(uv.KeySpace, 0)
	a.waitFor("heard nothing") // the fake wrote no audio
	if n := voiceKeysStarts(marker); n != 1 {
		t.Fatalf("recorder started %d times, want 1\nscreen:\n%s", n, a.text())
	}
}

func TestVoiceKeysHoldBurstRecords(t *testing.T) {
	t.Parallel()
	a, marker := voiceKeysStart(t, true)
	a.voiceKeysSlash("/voice hold")
	a.waitFor(voiceKeysChip)
	a.typeText("x")
	a.voiceKeysComposerHas("x", "draft x")
	a.typeText("   ") // key repeat: the third rapid press is a hold
	// "heard nothing" is only reachable through startVoice → stopVoice.
	// x/vt follows every press with a release and voice mode asks for
	// release events, so the take ends at once — often before the fake
	// rec has logged its start; hence at most one, not exactly one.
	a.waitFor("heard nothing")
	if n := voiceKeysStarts(marker); n > 1 {
		t.Fatalf("held space started the recorder %d times, want at most 1\nscreen:\n%s", n, a.text())
	}
	if c := strings.TrimRight(a.voiceKeysComposer(), " "); c != "> x" {
		t.Fatalf("warmup spaces left in the composer: %q\nscreen:\n%s", c, a.text())
	}
}
