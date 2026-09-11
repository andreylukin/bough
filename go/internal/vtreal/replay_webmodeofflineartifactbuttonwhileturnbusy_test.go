package vtreal

// Surface "web-mode-offline-artifact-button-while-turn-busy": the web
// row on a reserved loopback port, a published page with a button, and
// the button pressed (an HTTP POST to /answers, as the renderer sends
// it) while the agent is mid-turn. Turn 2 is a slow text-only stream,
// so nothing may land the notice inside it: the press must queue, wake
// exactly one follow-up turn after turn 2's done, and the page must
// report the state it was sent. Counting is deterministic: each spare
// tape turn prints a numbered WOKE marker, so every wake shows once.

import (
	"fmt"
	"net"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const webmodeofflineartifactbuttonwhileturnbusyProgram = `root = Card([head, ask])
head = CardHeader("Pick one", "busy")
ask = Buttons([Button("Keep SQLite"), Button("Try DuckDB")])`

func TestWebModeOfflineArtifactButtonWhileTurnBusy(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close()

	words := make([]string, 150)
	for i := range words {
		words[i] = fmt.Sprintf("w%03d", i)
	}
	words[0], words[len(words)-1] = "STORY-BEGINS", "STORY-ENDS"
	publish := jsBlock(fmt.Sprintf("console.log(tools.artifact(\"smoke\", %q))", webmodeofflineartifactbuttonwhileturnbusyProgram))
	turns := [][]string{{publish, stopReply("Published.")}, {stopReply(strings.Join(words, " "))}}
	for i := 1; i <= 3; i++ { // spare turns: a second wake would print WOKE-2
		turns = append(turns, []string{jsBlock(fmt.Sprintf(`console.log("WOKE-%d", tools.artifactAnswers("smoke"))`, i)), stopReply("noted")})
	}
	tape := e2eTape(t, turns...)
	yml := e2eConfig(t, tape, addr)
	slow := strings.Replace(yml, fmt.Sprintf("config: {file: %q}", tape), fmt.Sprintf("config: {file: %q, delay_ms: 50}", tape), 1)
	if slow == yml {
		t.Fatal("replay llm row not found; cannot slow the stream")
	}
	p := startHeadless(t, home, slow, filepath.Join(home, "opened.log"))

	p.send("publish the page")
	out := p.waitDone(1)
	m := artifactURLRe.FindStringSubmatch(out)
	if m == nil {
		t.Fatalf("no artifact URL in the publish result:\n%s", p.screen())
	}
	url := m[0]

	p.send("tell me a long story")
	// Headless prints the reply only at its done, so the press is timed:
	// 150 words at 50 ms stream for ~7.5 s; 1 s in is mid-turn.
	time.Sleep(time.Second)
	r, err := http.Post(url+"/answers", "application/json", strings.NewReader(
		`{"kind":"action","value":{"type":"continue_conversation","message":"Keep SQLite"},"state":{"pick":"sqlite"}}`))
	if err != nil {
		t.Fatal(err)
	}
	r.Body.Close()
	if r.StatusCode != 200 {
		t.Fatalf("answers POST = %d", r.StatusCode)
	}
	if n := strings.Count("\n"+p.stdout(), "\n[done]"); n != 1 {
		t.Fatalf("press was not mid-turn: %d turns done at the press (stream too fast)", n)
	}

	out = p.waitDone(3)
	t.Run("queued not interleaved", func(t *testing.T) {
		end2 := strings.Index(out, "STORY-ENDS")
		if end2 < 0 {
			t.Fatalf("turn 2 reply cut short:\n%s", p.screen())
		}
		// Every trace of the press must come after turn 2's reply ended.
		// (The button label is in turn 1's program; "pick" is only in the answer.)
		for _, needle := range []string{"[job]", `"pick"`, "WOKE-1"} {
			if i := strings.Index(out, needle); i >= 0 && i < end2 {
				t.Errorf("%q landed inside turn 2 (at %d, reply ends at %d):\n%s", needle, i, end2, p.screen())
			}
		}
		if !strings.Contains(out, "WOKE-1") || !strings.Contains(out, `"pick": "sqlite"`) {
			t.Errorf("the woken turn did not read the answer:\n%s", p.screen())
		}
	})
	t.Run("exactly one follow-up turn", func(t *testing.T) {
		time.Sleep(5 * time.Second) // past two more 2 s sweeps
		out := p.stdout()
		if n := strings.Count(out, "[result] WOKE-"); n != 1 {
			t.Errorf("press woke the agent %d times, want 1:\n%s", n, p.screen())
		}
		if n := strings.Count("\n"+out, "\n[done]"); n != 3 {
			t.Errorf("%d turns ran, want 3 (publish, story, one wake):\n%s", n, p.screen())
		}
	})
	t.Run("page reports state", func(t *testing.T) {
		code, _, body := httpGet(t, url+"/answers")
		if code != 200 || !strings.Contains(body, `"pick":"sqlite"`) || strings.Count(body, `"kind":"action"`) != 1 || !strings.Contains(body, "Keep SQLite") {
			t.Errorf("GET answers = %d %s", code, body)
		}
	})
}
