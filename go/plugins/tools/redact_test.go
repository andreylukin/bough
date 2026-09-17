package tools

import (
	"strings"
	"testing"

	iorb "github.com/andreylukin/bough/internal/orb"
)

// redactOrb is a fake orb whose project has one resolved secret.
type redactOrb struct{ fakeOrb }

func (o *redactOrb) Redactor() *iorb.Redactor {
	return iorb.NewRedactor(map[string]string{"API_KEY": "sk-live-0123456789"})
}

// A secret printed by a command never reaches the tool result, the error
// text of a failed command, or a background job's output, even when the
// value is written a few bytes at a time.
func TestBashRedactsOrbSecrets(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	o := &redactOrb{fakeOrb{root: t.TempDir()}}
	s.project = &projectMode{slug: "demo", orb: func() (orbExec, error) { return o, nil }}
	s.jobs.project = s.project
	out, err := s.bash("echo key=sk-live-0123456789")
	if err != nil || strings.Contains(out, "sk-live") || !strings.Contains(out, "key=[redacted:API_KEY]") {
		t.Fatalf("bash = %q, %v", out, err)
	}
	_, err = s.bash("printf 'sk-live-%s\\n' 0123456789; exit 2")
	if err == nil || strings.Contains(err.Error(), "sk-live-0") || !strings.Contains(err.Error(), "[redacted:API_KEY]") {
		t.Fatalf("failed bash error = %v", err)
	}
	b, err := s.jobs.start("for c in sk- live -012 3456 789; do printf %s $c; sleep 0.05; done; echo", 60e9, "")
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the job to finish", func() bool { b.mu.Lock(); defer b.mu.Unlock(); return b.done })
	b.mu.Lock()
	got := b.output()
	b.mu.Unlock()
	if strings.Contains(got, "sk-live") || !strings.Contains(got, "[redacted:API_KEY]") {
		t.Fatalf("job output %q", got)
	}
}
