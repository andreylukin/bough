package ui

// Voice dictation, Claude Code's shape: /voice turns it on, then Space
// is the push-to-talk key. Hold mode (default) records while Space is
// held — a held key is seen as rapid key-repeat presses, so recording
// starts after a short warmup (the warmup spaces are removed) and
// stops when the repeats stop or, on a terminal that reports key
// events (kitty protocol), on the release. Tap mode: Space on an empty
// composer starts recording, Space again stops it, and a transcript of
// three or more words is sent. Either way the text lands at the
// cursor, so typing and speech mix in one prompt.
//
// Audio comes from sox's rec, ffmpeg or arecord — whichever is
// installed — as 16 kHz mono WAV, and goes to the llm row's provider
// for transcription. Only a provider with an audio endpoint can do
// that (llm.Transcriber: OpenAI today); /voice says so otherwise.

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"syscall"
	"time"

	tea "charm.land/bubbletea/v2"
)

const (
	// spaceRepeatGap is the longest pause between two Space presses
	// that still reads as key repeat (macOS's slowest initial delay
	// is ~2 s, but the default is 225 ms; typed double spaces are
	// slower than this too).
	spaceRepeatGap = 400 * time.Millisecond
	// spaceHoldPresses is how many rapid presses make a hold: the
	// first two type spaces (removed once the hold is recognised).
	spaceHoldPresses = 3
	// voiceReleaseGap is the silence in repeats that means the key
	// went up on a terminal without release events.
	voiceReleaseGap = 350 * time.Millisecond
	voiceTick       = 100 * time.Millisecond
	voiceMaxTake    = 2 * time.Minute
	voiceTapWords   = 3 // tap mode sends a transcript this long
)

type voiceState struct {
	mode      string // "" (off), "hold", "tap"
	recording bool
	busy      bool // stopping + transcribing
	rec       *recorder
	startedAt time.Time
	lastSpace time.Time // last Space press: the burst and release clocks
	burst     int       // rapid Space presses in a row
}

// voiceMsg delivers a finished take: its transcript or the error.
type voiceMsg struct {
	text string
	err  error
}

type voiceTickMsg struct{}

// setVoice performs /voice: mode "" toggles, else sets hold/tap/off.
func (m *model) setVoice(mode string, cfg *uiCfg) tea.Cmd {
	if mode == "" {
		if m.v.mode != "" {
			mode = "off"
		} else if cfg.voiceMode != "" {
			mode = cfg.voiceMode
		} else {
			mode = "hold"
		}
	}
	if mode == "off" {
		m.v.mode = ""
		m.noteSystem("voice off")
		return m.cancelVoice()
	}
	if cfg.voice == nil {
		m.errorBlock(fmt.Sprintf("voice: %s has no transcription endpoint — dictation needs an llm row on llm-openai (OPENAI_API_KEY)", cfg.llmPlugin))
		return nil
	}
	tool := recorderTool()
	if tool == "" {
		m.errorBlock("voice: no recorder found — install sox (`brew install sox`), ffmpeg, or alsa-utils")
		return nil
	}
	m.v.mode = mode
	how := "hold space to speak, release to stop"
	if mode == "tap" {
		how = "space on an empty composer starts, space again stops and sends"
	}
	m.noteSystem(fmt.Sprintf("voice on (%s): %s · via %s + %s · /voice off", mode, how, tool, cfg.llmPlugin))
	return nil
}

func (m *model) errorBlock(text string) {
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "error", text: text})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
}

// voiceKey sees every key before the composer. It reports whether the
// key was the push-to-talk key and consumed.
func (m *model) voiceKey(key string, msg tea.KeyPressMsg, cfg *uiCfg) (bool, tea.Cmd) {
	if m.v.mode == "" || m.inspecting || m.pal.open {
		return false, nil
	}
	now := time.Now()
	if m.v.recording {
		if key == "space" {
			m.v.lastSpace = now
			m.flash = m.voiceFlash()
			if m.v.mode == "tap" {
				return true, m.stopVoice(cfg)
			}
			return true, nil // a repeat of the held key
		}
		return false, nil
	}
	if key != "space" {
		m.v.burst = 0
		return false, nil
	}
	if m.v.busy || m.pendingAsk != "" { // a pending ask owns the composer
		return false, nil
	}
	if m.v.mode == "tap" {
		if strings.TrimSpace(m.input.Value()) != "" {
			return false, nil // composing: a space is a space
		}
		return true, m.startVoice()
	}
	if now.Sub(m.v.lastSpace) < spaceRepeatGap {
		m.v.burst++
	} else {
		m.v.burst = 1
	}
	m.v.lastSpace = now
	if !msg.IsRepeat && m.v.burst < spaceHoldPresses {
		return false, nil // a typed space, until the repeats prove otherwise
	}
	// The warmup presses typed spaces; take them back.
	for range m.v.burst - 1 {
		m.input, _ = m.input.Update(tea.KeyPressMsg{Code: tea.KeyBackspace})
	}
	m.v.burst = 0
	return true, m.startVoice()
}

// voiceRelease handles a key-release event (terminals that report
// them): Space up ends a hold-mode take at once.
func (m *model) voiceRelease(msg tea.KeyReleaseMsg, cfg *uiCfg) tea.Cmd {
	if m.v.recording && m.v.mode == "hold" && msg.Code == tea.KeySpace {
		return m.stopVoice(cfg)
	}
	return nil
}

func (m *model) startVoice() tea.Cmd {
	rec, err := startRecorder()
	if err != nil {
		m.flash = "voice: " + err.Error()
		return nil
	}
	m.v.rec = rec
	m.v.recording = true
	m.v.startedAt = time.Now()
	m.v.lastSpace = m.v.startedAt
	m.flash = m.voiceFlash()
	return tea.Tick(voiceTick, func(time.Time) tea.Msg { return voiceTickMsg{} })
}

// voiceFlash is the status-bar line while recording: elapsed and how
// to stop.
func (m *model) voiceFlash() string {
	el := time.Since(m.v.startedAt).Round(time.Second)
	how := "release space to stop"
	if m.v.mode == "tap" {
		how = "space to stop"
	}
	return fmt.Sprintf("● %d:%02d recording · %s", int(el.Minutes()), int(el.Seconds())%60, how)
}

// voiceTicked keeps the elapsed fresh and ends a hold-mode take once
// the repeats stop; either mode ends at voiceMaxTake.
func (m *model) voiceTicked(cfg *uiCfg) tea.Cmd {
	if !m.v.recording {
		return nil
	}
	now := time.Now()
	if now.Sub(m.v.startedAt) > voiceMaxTake || (m.v.mode == "hold" && now.Sub(m.v.lastSpace) > voiceReleaseGap) {
		return m.stopVoice(cfg)
	}
	m.flash = m.voiceFlash()
	return tea.Tick(voiceTick, func(time.Time) tea.Msg { return voiceTickMsg{} })
}

// stopVoice ends the take and transcribes it off the UI goroutine.
func (m *model) stopVoice(cfg *uiCfg) tea.Cmd {
	rec := m.v.rec
	m.v.rec, m.v.recording, m.v.busy = nil, false, true
	m.flash = "transcribing…"
	transcriber := cfg.voice
	return func() tea.Msg {
		wav, err := rec.stop()
		if err != nil {
			return voiceMsg{err: err}
		}
		if len(wav) == 0 {
			return voiceMsg{}
		}
		if transcriber == nil {
			return voiceMsg{err: errors.New("no transcriber")}
		}
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		text, err := transcriber.Transcribe(ctx, wav, "")
		return voiceMsg{text: text, err: err}
	}
}

// cancelVoice drops a take in progress (voice off, quit).
func (m *model) cancelVoice() tea.Cmd {
	rec := m.v.rec
	m.v.rec, m.v.recording = nil, false
	if rec == nil {
		return nil
	}
	return func() tea.Msg { rec.stop(); return nil }
}

// finishVoice puts the transcript at the cursor; tap mode sends a
// real sentence.
func (m model) finishVoice(msg voiceMsg) (tea.Model, tea.Cmd) {
	m.v.busy = false
	if msg.err != nil {
		m.flash = "voice: " + msg.err.Error()
		return m, nil
	}
	if msg.text == "" {
		m.flash = "voice: heard nothing"
		return m, nil
	}
	if v := m.input.Value(); v != "" && !strings.HasSuffix(v, " ") && !strings.HasSuffix(v, "\n") {
		m.input.InsertString(" ")
	}
	m.input.InsertString(msg.text)
	m.syncPalette()
	m.layoutComposer()
	words := len(strings.Fields(msg.text))
	m.flash = "dictated " + plural(words, "word")
	if m.v.mode == "tap" && words >= voiceTapWords {
		return m.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	}
	return m, nil
}

// --- recording ---

// recorder is one running capture writing WAV to path.
type recorder struct {
	cmd  *exec.Cmd
	path string
	stub func() ([]byte, error) // tests: stop returns this instead
}

// startRecorder begins a capture. A package var so tests can stub it.
var startRecorder = startRecording

// recorderTool names the first available capture tool, "" when none.
func recorderTool() string {
	for _, t := range []string{"rec", "ffmpeg", "arecord"} {
		if _, err := exec.LookPath(t); err == nil {
			if t == "ffmpeg" && runtime.GOOS == "windows" {
				continue
			}
			return t
		}
	}
	return ""
}

func startRecording() (*recorder, error) {
	f, err := os.CreateTemp("", "bough-voice-*.wav")
	if err != nil {
		return nil, err
	}
	path := f.Name()
	f.Close()
	var cmd *exec.Cmd
	switch recorderTool() {
	case "rec":
		cmd = exec.Command("rec", "-q", "-r", "16000", "-c", "1", "-b", "16", path)
	case "ffmpeg":
		in := []string{"-f", "pulse", "-i", "default"}
		if runtime.GOOS == "darwin" {
			in = []string{"-f", "avfoundation", "-i", ":0"}
		}
		args := append([]string{"-y", "-loglevel", "error"}, in...)
		args = append(args, "-ar", "16000", "-ac", "1", path)
		cmd = exec.Command("ffmpeg", args...)
	case "arecord":
		cmd = exec.Command("arecord", "-q", "-f", "S16_LE", "-r", "16000", "-c", "1", path)
	default:
		os.Remove(path)
		return nil, errors.New("no recorder found (install sox, ffmpeg or alsa-utils)")
	}
	cmd.Stdin = nil
	if err := cmd.Start(); err != nil {
		os.Remove(path)
		return nil, err
	}
	return &recorder{cmd: cmd, path: path}, nil
}

// stop ends the capture (SIGINT lets every tool finish the WAV header)
// and returns the file's bytes.
func (r *recorder) stop() ([]byte, error) {
	if r.stub != nil {
		return r.stub()
	}
	defer os.Remove(r.path)
	r.cmd.Process.Signal(syscall.SIGINT)
	done := make(chan error, 1)
	go func() { done <- r.cmd.Wait() }()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		r.cmd.Process.Kill()
		<-done
	}
	data, err := os.ReadFile(r.path)
	if err != nil {
		return nil, err
	}
	if len(data) < 1024 { // a header and next to no samples
		return nil, nil
	}
	return data, nil
}
