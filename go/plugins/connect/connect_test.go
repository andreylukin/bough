package connect

// /connect records a provider key and switches to it. The one thing it
// must never do is let the key escape into the transcript or the
// history file, so that is what most of this checks.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

const testKey = "sk-or-v1-SECRETVALUE"

func envFile(t *testing.T) string {
	t.Helper()
	return filepath.Join(t.TempDir(), "env")
}

func TestListShowsStateNotKeys(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", testKey)
	t.Setenv("ANTHROPIC_API_KEY", "")
	out, err := run(envFile(t), nil, "")
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, testKey) {
		t.Fatalf("a listing must never print a key:\n%s", out)
	}
	if !strings.Contains(out, "openrouter") || !strings.Contains(out, "set") {
		t.Errorf("a configured provider should read as set:\n%s", out)
	}
	if !strings.Contains(out, "anthropic") {
		t.Errorf("every provider should be listed:\n%s", out)
	}
}

func TestWritesKeyAndSwitches(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "")
	path := envFile(t)
	var sets []string
	set := func(kv ...string) error { sets = append(sets, kv...); return nil }

	out, err := run(path, set, "openrouter "+testKey)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out, testKey) {
		t.Fatalf("the confirmation must not repeat the key:\n%s", out)
	}
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(b), "OPENROUTER_API_KEY="+testKey) {
		t.Errorf("key not recorded: %q", b)
	}
	if os.Getenv("OPENROUTER_API_KEY") != testKey {
		t.Error("the running process should pick the key up without a restart")
	}
	if len(sets) != 2 || sets[0] != "llm.plugin=llm-openrouter" {
		t.Errorf("the llm row should have been switched, got %v", sets)
	}
	// Credentials file, credentials permissions.
	st, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if st.Mode().Perm() != 0o600 {
		t.Errorf("env file mode = %v, want 0600", st.Mode().Perm())
	}
}

// Setting a key again replaces the line rather than appending a second
// one, and leaves other providers alone.
func TestRewriteKeepsOtherKeys(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "")
	path := envFile(t)
	if err := os.WriteFile(path, []byte("ANTHROPIC_API_KEY=keep-me\nOPENROUTER_API_KEY=old\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := run(path, nil, "openrouter "+testKey); err != nil {
		t.Fatal(err)
	}
	b, _ := os.ReadFile(path)
	got := string(b)
	if strings.Contains(got, "old") {
		t.Errorf("the previous key should be replaced: %q", got)
	}
	if !strings.Contains(got, "ANTHROPIC_API_KEY=keep-me") {
		t.Errorf("another provider's key must survive: %q", got)
	}
	if n := strings.Count(got, "OPENROUTER_API_KEY="); n != 1 {
		t.Errorf("want one OPENROUTER line, got %d: %q", n, got)
	}
}

func TestUnknownProvider(t *testing.T) {
	if _, err := run(envFile(t), nil, "hal9000 key"); err == nil {
		t.Fatal("an unknown provider should be an error naming the real ones")
	}
}

func TestProviderWithoutKeySaysWhatToRun(t *testing.T) {
	t.Setenv("CEREBRAS_API_KEY", "")
	out, err := run(envFile(t), nil, "cerebras")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "/connect cerebras <key>") {
		t.Errorf("it should say exactly what to run:\n%s", out)
	}
}

// A newline in a key would corrupt the env file into a second setting.
func TestRejectsMultilineKey(t *testing.T) {
	if err := writeKey(envFile(t), "OPENROUTER_API_KEY", "abc\nEVIL=1"); err == nil {
		t.Fatal("a key containing a newline must be refused")
	}
}

// The command registers as Secret, which is what makes the ui redact
// its arguments before echoing or recording them.
func TestRegistersAsSecret(t *testing.T) {
	ctx := kernel.NewContext()
	reg := commands.NewRegistry()
	ctx.Provide("commands", reg)
	if err := (plugin{}).Apply(ctx, map[string]any{"env_file": envFile(t)}); err != nil {
		t.Fatal(err)
	}
	found := false
	for _, in := range reg.List() {
		if in.Name == "connect" {
			found = true
			if !in.Secret {
				t.Error("/connect takes an API key and must be marked Secret")
			}
		}
	}
	if !found {
		t.Fatal("/connect not registered")
	}
	ctx.Unmount()
	if _, err := reg.Run("connect", ""); err == nil {
		t.Error("unmount should unregister the command")
	}
}

// A key pasted from a shell `export KEY="..."` line arrives quoted.
// Storing the quotes sends the provider `Bearer "sk-…"` — a 401 that
// blames the key.
func TestStripsSurroundingQuotes(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "")
	path := envFile(t)
	for _, quoted := range []string{`"` + testKey + `"`, "'" + testKey + "'"} {
		if _, err := run(path, nil, "openrouter "+quoted); err != nil {
			t.Fatal(err)
		}
		if got := os.Getenv("OPENROUTER_API_KEY"); got != testKey {
			t.Errorf("quotes should be stripped, got %q", got)
		}
		b, _ := os.ReadFile(path)
		if !strings.Contains(string(b), "OPENROUTER_API_KEY="+testKey+"\n") {
			t.Errorf("the file should hold the bare key: %q", b)
		}
	}
}
