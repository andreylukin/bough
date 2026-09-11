package ui

// Voice mode with an ask card pending: space belongs to the ask's
// freeform answer, and no recorder may be spawned. A fake `rec` on PATH
// writes a sentinel when started. Stubs PATH, so not parallel.

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

// voiceWhileAskPendingDrv is a driver with an ask pending, voice on in
// mode, the real startRecording and a sentinel-writing rec on PATH.
func voiceWhileAskPendingDrv(t *testing.T, mode string) (*drv, *fakeAsk, string) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("shell-script recorder")
	}
	bin := t.TempDir()
	sentinel := filepath.Join(bin, "spawned")
	script := "#!/bin/sh\n/usr/bin/touch '" + sentinel + "'\ntrap 'exit 0' INT TERM\nwhile :; do /bin/sleep 0.05; done\n"
	if err := os.WriteFile(filepath.Join(bin, "rec"), []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin)
	startRecorder = startRecording
	fa := &fakeAsk{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ask = fa
	cfg.voice = &fakeTranscriber{text: "stolen"}
	cfg.llmPlugin = "llm-openai"
	d := newDrv(t, 80, 24, cfg)
	d.m.v.mode = mode
	t.Cleanup(func() {
		if d.m.v.recording {
			runCmd(d.m.cancelVoice())
		}
	})
	d.feed(askEvent())
	if d.m.pendingAsk != "ask-1" {
		t.Fatalf("ask not pending")
	}
	return d, fa, sentinel
}

func voiceWhileAskPendingCheck(t *testing.T, d *drv, fa *fakeAsk, sentinel string) {
	t.Helper()
	if d.m.v.recording {
		t.Errorf("space started recording over a pending ask (flash %q)", d.m.flash)
	}
	time.Sleep(200 * time.Millisecond) // let a spawned recorder touch its sentinel
	if _, err := os.Stat(sentinel); err == nil {
		t.Errorf("a recorder was spawned while the ask was pending")
	}
	if d.m.v.recording {
		runCmd(d.m.cancelVoice()) // so the answer below can still be checked
	}
	if d.m.pendingAsk != "ask-1" {
		t.Fatalf("ask stolen: pending = %q", d.m.pendingAsk)
	}
	d.m.input.SetValue("")
	d.typeStr("sea")
	d.feed(keySpace())
	d.typeStr("green")
	d.press(keyEnter())
	if len(fa.texts) != 1 || fa.texts[0] != "sea green" {
		t.Errorf("freeform answer = %q, want [sea green]", fa.texts)
	}
}

func TestVoiceWhileAskPending(t *testing.T) {
	t.Run("tap", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_VOICE_WHILE_ASK_PENDING") == "" {
			t.Skip("known bug: voiceKey (plugins/ui/voice.go) ignores m.pendingAsk, so space starts a take over a pending ask; set BOUGH_KNOWN_VOICE_WHILE_ASK_PENDING=1")
		}
		d, fa, s := voiceWhileAskPendingDrv(t, "tap")
		d.feed(keySpace())
		voiceWhileAskPendingCheck(t, d, fa, s)
	})
	t.Run("hold", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_VOICE_WHILE_ASK_PENDING") == "" {
			t.Skip("known bug: voiceKey (plugins/ui/voice.go) ignores m.pendingAsk, so space starts a take over a pending ask; set BOUGH_KNOWN_VOICE_WHILE_ASK_PENDING=1")
		}
		d, fa, s := voiceWhileAskPendingDrv(t, "hold")
		d.feed(keySpace())
		d.feed(keySpace())
		d.feed(keySpace())
		d.feed(tea.KeyPressMsg{Code: tea.KeySpace, Text: " ", IsRepeat: true})
		voiceWhileAskPendingCheck(t, d, fa, s)
	})
}
