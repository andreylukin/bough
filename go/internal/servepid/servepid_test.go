package servepid

import (
	"os"
	"testing"
)

func TestParse(t *testing.T) {
	t.Parallel()
	pid, addr, dir, config, caps, err := Parse("4242 127.0.0.1:7683\t/Users/a/my code\t/x/bough.yml\tnew-session\n")
	if err != nil || pid != 4242 || addr != "127.0.0.1:7683" || dir != "/Users/a/my code" || config != "/x/bough.yml" || caps != "new-session" {
		t.Fatalf("got %d %q %q %q %q %v", pid, addr, dir, config, caps, err)
	}
	if pid, addr, dir, _, _, err := Parse("7 localhost:1\n"); err != nil || pid != 7 || addr != "localhost:1" || dir != "" {
		t.Fatalf("short form: %d %q %q %v", pid, addr, dir, err)
	}
	for _, bad := range []string{"nonsense", "0 a:1", "-3 a:1", "x a:1"} {
		if _, _, _, _, _, err := Parse(bad); err == nil {
			t.Fatalf("Parse(%q) accepted", bad)
		}
	}
}

func TestAlive(t *testing.T) {
	t.Parallel()
	if !Alive(os.Getpid()) {
		t.Fatal("own pid not alive")
	}
	if Alive(0) || Alive(-1) || Alive(0x7FFFFFFE) {
		t.Fatal("impossible pid reported alive")
	}
}
