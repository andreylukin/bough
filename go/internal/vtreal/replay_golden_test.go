package vtreal

// Golden screenshots of the fixture tape: the settled screen after
// every turn, at three pane sizes, recorded under
// testdata/replay/golden/<cols>x<rows>-turn<N>.txt. Layout, wrapping
// and chrome changes then show up as a unified diff instead of a
// vague assertion.
//
//	go test ./internal/vtreal -run TestGolden -update   # rewrite them
//	BOUGH_GOLDEN_UPDATE=1 go test ./internal/vtreal -run TestGolden

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/replay"
)

var goldenUpdate = flag.Bool("update", false, "rewrite the golden screenshots under testdata/replay/golden")

var goldenSizes = [][2]int{{80, 24}, {100, 30}, {200, 50}}

// goldenVolatile masks what changes between runs: the temp $HOME the
// run was given, clock times and elapsed/cost chips.
var goldenVolatile = []struct {
	re   *regexp.Regexp
	with string
}{
	{regexp.MustCompile(`/(private/)?(var|tmp)/[^\s"']*`), "<TMP>"},
	{regexp.MustCompile(`\b\d+:\d\d(:\d\d)?\b`), "<TIME>"},
	{regexp.MustCompile(`\b\d+(\.\d+)?m?s\b`), "<DUR>"},
	{regexp.MustCompile(`\$\d+\.\d+`), "<COST>"},
	// The system prompt embeds the run's own cwd, so its length moves
	// with the temp directory name.
	{regexp.MustCompile(`\d+ chars in the system prompt`), "<N> chars in the system prompt"},
}

func goldenScrub(a *app, screen string) string {
	s := strings.ReplaceAll(screen, a.home, "<HOME>")
	for _, v := range goldenVolatile {
		s = v.re.ReplaceAllString(s, v.with)
	}
	return s
}

// goldenDiff is a minimal unified diff: the changed span between the
// common prefix and suffix, with three lines of context either side.
func goldenDiff(want, got string) string {
	w, g := strings.Split(want, "\n"), strings.Split(got, "\n")
	head := 0
	for head < len(w) && head < len(g) && w[head] == g[head] {
		head++
	}
	tail := 0
	for tail < len(w)-head && tail < len(g)-head && w[len(w)-1-tail] == g[len(g)-1-tail] {
		tail++
	}
	from := max(head-3, 0)
	var b strings.Builder
	fmt.Fprintf(&b, "@@ -%d,%d +%d,%d @@\n", from+1, len(w)-tail-from, from+1, len(g)-tail-from)
	for _, l := range w[from:head] {
		fmt.Fprintf(&b, " %s\n", l)
	}
	for _, l := range w[head : len(w)-tail] {
		fmt.Fprintf(&b, "-%s\n", l)
	}
	for _, l := range g[head : len(g)-tail] {
		fmt.Fprintf(&b, "+%s\n", l)
	}
	for _, l := range w[len(w)-tail : min(len(w)-tail+3, len(w))] {
		fmt.Fprintf(&b, " %s\n", l)
	}
	return b.String()
}

func goldenCheck(t *testing.T, a *app, path, got string) {
	t.Helper()
	if *goldenUpdate || os.Getenv("BOUGH_GOLDEN_UPDATE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(got+"\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("no golden %s (run with -update)\nscreen:\n%s", path, a.text())
	}
	want := strings.TrimSuffix(string(raw), "\n")
	if want != got {
		t.Errorf("golden %s differs:\n%s\nscreen:\n%s", path, goldenDiff(want, got), a.text())
	}
}

func TestGoldenScreens(t *testing.T) {
	t.Parallel()
	tape, err := filepath.Abs("testdata/replay/basic.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	tp, err := replay.Load(tape)
	if err != nil {
		t.Fatal(err)
	}
	for _, sz := range goldenSizes {
		name := fmt.Sprintf("%dx%d", sz[0], sz[1])
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, sz[0], sz[1], replayConfig(tape))
			turns := 0
			for _, in := range tp.Inputs {
				if strings.HasPrefix(in, "/") {
					continue // a command: never went to the model
				}
				a.typeText(in)
				a.key(uv.KeyEnter, 0)
				turns++
				if !a.waitDone(turns, 60*time.Second) {
					t.Fatalf("turn %d never finished:\n%s", turns, a.text())
				}
				got := goldenScrub(a, a.settled())
				path := filepath.Join("testdata", "replay", "golden", fmt.Sprintf("%s-turn%d.txt", name, turns))
				goldenCheck(t, a, path, got)
			}
		})
	}
}
