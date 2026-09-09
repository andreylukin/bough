package ui

// Voice dictation (voice.go) with a stubbed recorder and transcriber.
// These stub package vars, so they do not run in parallel.

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

type fakeTranscriber struct {
	text string
	got  []byte
}

func (f *fakeTranscriber) Transcribe(_ context.Context, wav []byte, _ string) (string, error) {
	f.got = wav
	return f.text, nil
}

func keySpace() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeySpace, Text: " "} }

// voiceDrv is a driver with a transcriber in the llm row, voice on in
// mode, and a recorder that "captures" wav.
func voiceDrv(t *testing.T, mode string, tr *fakeTranscriber, wav []byte) *drv {
	t.Helper()
	startRecorder = func() (*recorder, error) {
		return &recorder{stub: func() ([]byte, error) { return wav, nil }}, nil
	}
	t.Cleanup(func() { startRecorder = startRecording })
	cfg := cfgWith(t, nil, nil, nil)
	cfg.voice = tr
	cfg.llmPlugin = "llm-openai"
	d := newDrv(t, 80, 24, cfg)
	d.m.v.mode = mode
	return d
}

// finishTake runs the stop command and feeds the transcript back.
func finishTake(d *drv, cmd tea.Cmd) {
	for _, msg := range runCmd(cmd) {
		if _, ok := msg.(voiceMsg); ok {
			next, after := d.m.Update(msg)
			d.m = next.(model)
			runCmd(after) // tap mode's auto-send runs here
		}
	}
}

func TestVoiceOffWithoutTranscriber(t *testing.T) {
	d := defaultDrv(t)
	d.m.cfg.Load().llmPlugin = "llm-anthropic"
	d.m.setVoice("", d.m.cfg.Load())
	if d.m.v.mode != "" {
		t.Fatalf("voice turned on with no transcriber")
	}
	last := d.m.blocks[len(d.m.blocks)-1]
	if last.kind != "error" || !strings.Contains(last.text, "llm-anthropic has no transcription endpoint") {
		t.Fatalf("block = %+v", last)
	}
}

func TestVoiceHoldWarmupThenTake(t *testing.T) {
	if recorderTool() == "" {
		t.Skip("no recorder on this machine")
	}
	tr := &fakeTranscriber{text: "use the helper"}
	d := voiceDrv(t, "hold", tr, []byte("RIFFwav"))
	d.typeStr("refactor")
	// Two rapid presses type spaces (a typed space must still work);
	// the third is the hold.
	d.feed(keySpace())
	d.feed(keySpace())
	if got := d.m.input.Value(); got != "refactor  " {
		t.Fatalf("warmup draft = %q", got)
	}
	next, cmd := d.m.Update(keySpace())
	d.m = next.(model)
	if !d.m.v.recording || cmd == nil {
		t.Fatalf("third rapid space should start recording")
	}
	if got := d.m.input.Value(); got != "refactor" {
		t.Fatalf("warmup spaces not removed: %q", got)
	}
	if !strings.Contains(d.m.flash, "recording") {
		t.Errorf("flash = %q", d.m.flash)
	}
	// Repeats keep it going; the tick after the repeats stop ends it.
	d.feed(keySpace())
	d.m.v.lastSpace = time.Now().Add(-voiceReleaseGap - time.Millisecond)
	next, cmd = d.m.Update(voiceTickMsg{})
	d.m = next.(model)
	if d.m.v.recording || !d.m.v.busy {
		t.Fatalf("release should stop the take: %+v", d.m.v)
	}
	finishTake(d, cmd)
	if got := d.m.input.Value(); got != "refactor use the helper" {
		t.Fatalf("draft = %q", got)
	}
	if string(tr.got) != "RIFFwav" {
		t.Errorf("transcriber got %q", tr.got)
	}
	if d.m.flash != "dictated 3 words" {
		t.Errorf("flash = %q", d.m.flash)
	}
	if len(d.sent) != 0 {
		t.Errorf("hold mode must not send: %q", d.sent)
	}
}

func TestVoiceHoldReleaseEvent(t *testing.T) {
	if recorderTool() == "" {
		t.Skip("no recorder on this machine")
	}
	d := voiceDrv(t, "hold", &fakeTranscriber{text: "hi"}, []byte("RIFFwav"))
	d.feed(tea.KeyPressMsg{Code: tea.KeySpace, Text: " ", IsRepeat: true})
	if !d.m.v.recording {
		t.Fatalf("a repeat-flagged space (kitty protocol) should start recording at once")
	}
	next, cmd := d.m.Update(tea.KeyReleaseMsg{Code: tea.KeySpace})
	d.m = next.(model)
	if d.m.v.recording {
		t.Fatalf("release event should stop the take")
	}
	finishTake(d, cmd)
	if got := d.m.input.Value(); got != "hi" {
		t.Fatalf("draft = %q", got)
	}
}

func TestVoiceTapSendsASentence(t *testing.T) {
	if recorderTool() == "" {
		t.Skip("no recorder on this machine")
	}
	d := voiceDrv(t, "tap", &fakeTranscriber{text: "fix the failing test"}, []byte("RIFFwav"))
	d.feed(keySpace())
	if !d.m.v.recording {
		t.Fatalf("tap on an empty composer should start recording")
	}
	next, cmd := d.m.Update(keySpace())
	d.m = next.(model)
	finishTake(d, cmd)
	if len(d.sent) != 1 || d.sent[0] != "fix the failing test" {
		t.Fatalf("sent = %q", d.sent)
	}
	// A short transcript is inserted, not sent.
	d2 := voiceDrv(t, "tap", &fakeTranscriber{text: "hello"}, []byte("RIFFwav"))
	d2.feed(keySpace())
	next, cmd = d2.m.Update(keySpace())
	d2.m = next.(model)
	finishTake(d2, cmd)
	if len(d2.sent) != 0 || d2.m.input.Value() != "hello" {
		t.Fatalf("short transcript: sent=%q draft=%q", d2.sent, d2.m.input.Value())
	}
	// Composing: space is a space.
	d2.typeStr("x")
	d2.feed(keySpace())
	if d2.m.v.recording || d2.m.input.Value() != "hellox " {
		t.Fatalf("tap while composing: recording=%v draft=%q", d2.m.v.recording, d2.m.input.Value())
	}
}

func TestVoiceSilentTakeAndRecorderError(t *testing.T) {
	if recorderTool() == "" {
		t.Skip("no recorder on this machine")
	}
	d := voiceDrv(t, "tap", &fakeTranscriber{text: "x"}, nil)
	d.feed(keySpace())
	next, cmd := d.m.Update(keySpace())
	d.m = next.(model)
	finishTake(d, cmd)
	if d.m.flash != "voice: heard nothing" || d.m.input.Value() != "" {
		t.Fatalf("flash=%q draft=%q", d.m.flash, d.m.input.Value())
	}
	startRecorder = func() (*recorder, error) { return nil, errors.New("mic busy") }
	d.feed(keySpace())
	if d.m.v.recording || d.m.flash != "voice: mic busy" {
		t.Fatalf("recording=%v flash=%q", d.m.v.recording, d.m.flash)
	}
}

func TestVoiceCommandTogglesAndKeysListsSpace(t *testing.T) {
	if recorderTool() == "" {
		t.Skip("no recorder on this machine")
	}
	d := voiceDrv(t, "", &fakeTranscriber{}, nil)
	d.m.setVoice("", d.m.cfg.Load())
	if d.m.v.mode != "hold" {
		t.Fatalf("mode = %q", d.m.v.mode)
	}
	d.m.setVoice("tap", d.m.cfg.Load())
	if d.m.v.mode != "tap" {
		t.Fatalf("mode = %q", d.m.v.mode)
	}
	d.m.setVoice("", d.m.cfg.Load())
	if d.m.v.mode != "" {
		t.Fatalf("toggle should turn voice off, mode = %q", d.m.v.mode)
	}
	if !strings.Contains(keysText(d.m.cfg.Load()), "/voice") {
		t.Errorf("keys text should mention voice")
	}
}
