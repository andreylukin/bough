package vtreal

// Resuming a session recorded under a provider/model this config does
// not have: the transcript still renders, the status bar names the
// configured llm (never the recorded, unavailable model), the next turn
// goes to the configured replay llm, and its assistant entry records
// the model that actually answered.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// resumeSessionWithMissingModelSeed is a finished one-turn session
// answered by a provider/model no row here provides.
const resumeSessionWithMissingModelSeed = `{"seq":1,"at":"2026-09-10T11:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T11:00:01Z","kind":"input","data":{"text":"old question"}}
{"seq":3,"at":"2026-09-10T11:00:02Z","kind":"assistant","data":{"text":"` + "```stop\\nAnswer from the ghost model.\\n```" + `","model":"ghost-model-9","provider":"ghost"}}
{"seq":4,"at":"2026-09-10T11:00:02Z","kind":"done","data":{"text":""}}
`

// resumeSessionWithMissingModelTape answers the next turn and records
// the model the replay llm reports.
const resumeSessionWithMissingModelTape = `{"seq":1,"at":"2026-09-10T12:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T12:00:01Z","kind":"input","data":{"text":"new question"}}
{"seq":3,"at":"2026-09-10T12:00:02Z","kind":"assistant","data":{"text":"` + "```stop\\nAnswer from the replay model.\\n```" + `","model":"replay-model-1","provider":"replay"}}
{"seq":4,"at":"2026-09-10T12:00:02Z","kind":"done","data":{"text":""}}
`

func resumeSessionWithMissingModelWrite(t *testing.T, name, body string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// resumeSessionWithMissingModelStatus is the status bar row.
func resumeSessionWithMissingModelStatus(a *app) string {
	ls := strings.Split(a.settled(), "\n")
	for i := len(ls) - 1; i >= 0; i-- {
		if strings.Contains(ls[i], "? keys") {
			return ls[i]
		}
	}
	return ""
}

func TestResumeSessionWithMissingModel(t *testing.T) {
	t.Parallel()
	log := resumeSessionWithMissingModelWrite(t, "session.jsonl", resumeSessionWithMissingModelSeed)
	tape := resumeSessionWithMissingModelWrite(t, "tape.jsonl", resumeSessionWithMissingModelTape)

	a := startCfg(t, 100, 30, resumeConfig(tape, log))
	a.check("resumed boot")
	a.waitFor("resumed ")

	t.Run("transcript renders", func(t *testing.T) {
		s := a.settled()
		for _, want := range []string{"old question", "Answer from the ghost model."} {
			if !strings.Contains(s, want) {
				t.Fatalf("resumed transcript is missing %q:\n%s", want, s)
			}
		}
	})

	t.Run("status bar names the configured llm", func(t *testing.T) {
		bar := resumeSessionWithMissingModelStatus(a)
		if strings.Contains(bar, "ghost") {
			t.Fatalf("status bar claims the unavailable recorded model: %q\n%s", bar, a.text())
		}
		if !strings.Contains(bar, "replay") {
			t.Fatalf("status bar names no fallback model/provider: %q\n%s", bar, a.text())
		}
	})

	resumeSend(t, a, log, "new question", 2)
	a.check("after next turn")

	t.Run("next turn goes to the configured llm", func(t *testing.T) {
		a.waitFor("Answer from the replay model.")
		if bar := resumeSessionWithMissingModelStatus(a); !strings.Contains(bar, "replay-model-1") {
			t.Fatalf("status bar does not name the model that answered: %q\n%s", bar, a.text())
		}
	})

	t.Run("entries record the actual model", func(t *testing.T) {
		entries, err := history.Read(log)
		if err != nil {
			t.Fatal(err)
		}
		var got [][2]any
		for _, e := range entries {
			if e.Kind == "assistant" {
				got = append(got, [2]any{e.Data["model"], e.Data["provider"]})
			}
		}
		want := [][2]any{{"ghost-model-9", "ghost"}, {"replay-model-1", "replay"}}
		if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
			t.Fatalf("assistant (model, provider) = %v, want %v", got, want)
		}
	})
}
