package ask

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

// secretEnv makes a temp home with project "demo" and swaps the
// keychain write seam; never the user's keychain. Not parallel: the
// seams are package globals.
func secretEnv(t *testing.T) (home string, stored map[string]string) {
	t.Helper()
	home = t.TempDir()
	dir := filepath.Join(home, ".bough", "projects", "demo")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "project.yml"), []byte("repos:\n  - path: "+home+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	stored = map[string]string{}
	oldW, oldH := secrets.KeychainWrite, userHome
	secrets.KeychainWrite = func(service, value string) error { stored[service] = value; return nil }
	userHome = func() (string, error) { return home, nil }
	t.Cleanup(func() { secrets.KeychainWrite, userHome = oldW, oldH })
	return home, stored
}

// answerNext answers the next emitted ask with text.
func answerNext(t *testing.T, a *Asker, text string) {
	t.Helper()
	orig := a.emit
	a.emit = func(ev Event) {
		orig(ev)
		if !ev.Secret {
			t.Errorf("secret ask event lacks Secret: %+v", ev)
		}
		go func() {
			if err := a.Answer(ev.ID, text); err != nil {
				t.Errorf("answer: %v", err)
			}
		}()
	}
}

func TestSecretStoresAndRedacts(t *testing.T) {
	home, stored := secretEnv(t)
	_, a, code, hist, _ := mount(t, nil)
	fn, ok := code.tools["secret"].(func(string, string, ...string) (string, error))
	if !ok {
		t.Fatalf("tools.secret not registered: %T", code.tools["secret"])
	}
	const value = "https://user:pw@devpi.example/simple"
	answerNext(t, a, value+" \t\n") // pasted with trailing whitespace
	got, err := fn("DEVPI_URL", "uv needs the index", "demo")
	if err != nil {
		t.Fatalf("secret: %v", err)
	}
	// Called outside the demo project session: no "next command" promise.
	if want := "stored DEVPI_URL as keychain:bough/demo/DEVPI_URL; applies to project demo's next command or session"; got != want {
		t.Fatalf("got %q, want %q", got, want)
	}
	if stored["bough/demo/DEVPI_URL"] != value {
		t.Fatalf("keychain = %v", stored)
	}
	for _, e := range hist.all() {
		if strings.Contains(fmt.Sprint(e.Data), value) {
			t.Fatalf("history %s holds the value: %v", e.Kind, e.Data)
		}
	}
	es := hist.all()
	if len(es) != 2 || es[0].Data["secret"] != true || es[1].Data["text"] != "[secret stored]" || es[1].Data["secret"] != true {
		t.Fatalf("history entries = %+v", es)
	}
	p, err := projectdef.Load(home, "demo")
	if err != nil {
		t.Fatal(err)
	}
	if p.Def.Secrets["DEVPI_URL"] != "keychain:bough/demo/DEVPI_URL" {
		t.Fatalf("project secrets = %v", p.Def.Secrets)
	}
	b, _ := os.ReadFile(filepath.Join(home, ".bough", "projects", "demo", "project.yml"))
	if strings.Contains(string(b), value) {
		t.Fatalf("project.yml holds the value")
	}
}

func TestSecretStoreErrorHasNoValue(t *testing.T) {
	_, _ = secretEnv(t)
	const value = "tok-abc123"
	secrets.KeychainWrite = func(service, v string) error { return errors.New("boom " + v) }
	_, a, code, _, _ := mount(t, nil)
	fn := code.tools["secret"].(func(string, string, ...string) (string, error))
	answerNext(t, a, value)
	_, err := fn("TOKEN", "why", "demo")
	if err == nil || !strings.HasPrefix(err.Error(), "secret: store failed:") || strings.Contains(err.Error(), value) {
		t.Fatalf("err = %v", err)
	}
}

func TestSecretDeclinedAndNoProject(t *testing.T) {
	_, stored := secretEnv(t)
	_, a, code, _, _ := mount(t, nil)
	fn := code.tools["secret"].(func(string, string, ...string) (string, error))
	if _, err := fn("TOKEN", "why"); err == nil || err.Error() != "secret: pass the project slug" {
		t.Fatalf("no project: err = %v", err)
	}
	for _, ans := range []string{"(declined)", ""} {
		answerNext(t, a, ans)
		if _, err := fn("TOKEN", "why", "demo"); err == nil || err.Error() != "secret: user declined" {
			t.Fatalf("answer %q: err = %v", ans, err)
		}
		a.emit = func(Event) {}
	}
	if len(stored) != 0 {
		t.Fatalf("declined ask stored %v", stored)
	}
	if _, err := fn("bad-name", "why", "demo"); err == nil {
		t.Fatal("invalid name accepted")
	}
}
