package ui

// Voice hold mode with the @ file picker open or an ask card pending:
// a held space (three rapid presses then a key repeat) belongs to the
// picker's filter / the ask's freeform answer. No take may start and no
// recorder may be spawned. A fake `rec` on PATH writes a sentinel when
// started. Stubs PATH and cwd, so not parallel.

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

// voiceHoldSpaceInPickerAndAskDrv is a driver in voice hold mode with
// the real startRecording and a sentinel-writing rec on PATH.
func voiceHoldSpaceInPickerAndAskDrv(t *testing.T) (*drv, *fakeAsk, string) {
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
	d.m.v.mode = "hold"
	t.Cleanup(func() {
		if d.m.v.recording {
			runCmd(d.m.cancelVoice())
		}
	})
	return d, fa, sentinel
}

// voiceHoldSpaceInPickerAndAskHold sends what a held space looks like:
// three rapid presses, then a key repeat.
func voiceHoldSpaceInPickerAndAskHold(d *drv) {
	d.feed(keySpace())
	d.feed(keySpace())
	d.feed(keySpace())
	d.feed(tea.KeyPressMsg{Code: tea.KeySpace, Text: " ", IsRepeat: true})
}

func voiceHoldSpaceInPickerAndAskNoTake(t *testing.T, d *drv, sentinel string) {
	t.Helper()
	if d.m.v.recording {
		t.Errorf("held space started a take (flash %q)", d.m.flash)
		runCmd(d.m.cancelVoice())
	}
	time.Sleep(200 * time.Millisecond) // let a spawned recorder touch its sentinel
	if _, err := os.Stat(sentinel); err == nil {
		t.Errorf("a recorder was spawned")
	}
}

const voiceHoldSpaceInPickerAndAskGate = "BOUGH_KNOWN_VOICE_HOLD_SPACE_IN_PICKER_AND_ASK"

func TestVoiceHoldSpaceInPickerAndAsk(t *testing.T) {
	t.Run("picker", func(t *testing.T) {
		if os.Getenv(voiceHoldSpaceInPickerAndAskGate) == "" {
			t.Skip("known bug: voiceKey (plugins/ui/voice.go) gates only on m.pal.open, not m.at.open, so a held space in the @ picker filter starts a take and backspaces the typed spaces away; set " + voiceHoldSpaceInPickerAndAskGate + "=1")
		}
		dir := t.TempDir()
		os.WriteFile(filepath.Join(dir, "main.go"), []byte("x"), 0o644)
		wd, _ := os.Getwd()
		os.Chdir(dir)
		t.Cleanup(func() { os.Chdir(wd) })
		d, _, sentinel := voiceHoldSpaceInPickerAndAskDrv(t)
		d.typeStr("read @ma")
		if !d.m.at.open {
			t.Fatal("@ picker not open")
		}
		voiceHoldSpaceInPickerAndAskHold(d)
		voiceHoldSpaceInPickerAndAskNoTake(t, d, sentinel)
		if got := d.m.input.Value(); got != "read @ma    " {
			t.Errorf("draft = %q, want the four spaces typed after the filter", got)
		}
	})
	t.Run("ask", func(t *testing.T) {
		if os.Getenv(voiceHoldSpaceInPickerAndAskGate) == "" {
			t.Skip("known bug: voiceKey (plugins/ui/voice.go) ignores m.pendingAsk, so a held space in the ask freeform box starts a take and eats the typed spaces; set " + voiceHoldSpaceInPickerAndAskGate + "=1")
		}
		d, fa, sentinel := voiceHoldSpaceInPickerAndAskDrv(t)
		d.feed(askEvent())
		if d.m.pendingAsk != "ask-1" {
			t.Fatal("ask not pending")
		}
		d.typeStr("sea")
		voiceHoldSpaceInPickerAndAskHold(d)
		voiceHoldSpaceInPickerAndAskNoTake(t, d, sentinel)
		d.typeStr("green")
		d.press(keyEnter())
		if len(fa.texts) != 1 || fa.texts[0] != "sea    green" {
			t.Errorf("freeform answer = %q, want [sea    green]", fa.texts)
		}
	})
}
