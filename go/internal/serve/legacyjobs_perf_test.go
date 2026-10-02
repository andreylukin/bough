package serve

import (
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestRunningJobsLargeLegacyOutput(t *testing.T) {
	t.Parallel()
	noise := strings.Repeat("unrelated tool output with an inline job 99 started in the background (limit): ignored\n", 10000)
	at := time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC)
	es := []history.Entry{
		{Seq: 1, At: at, Kind: "result", Data: map[string]any{"text": noise + "job 1 started in the background (limit 30m0s): go test ./...\n" + noise + "job 2 started in the background (limit): npm run dev"}},
		{Seq: 2, At: at.Add(time.Second), Kind: "job", Data: map[string]any{"text": "job 1 [exited 0] go test ./..."}},
		{Seq: 3, At: at.Add(2 * time.Second), Kind: "result", Data: map[string]any{"text": noise + "job 1 started in the background (limit): make test\n" + noise}},
	}
	want := []Job{{ID: 2, Cmd: "npm run dev", Started: at}, {ID: 1, Cmd: "make test", Started: at.Add(2 * time.Second)}}
	if got := RunningJobs(es, true); !reflect.DeepEqual(got, want) {
		t.Fatalf("jobs = %+v, want %+v", got, want)
	}
	if got := RunningJobs(es, false); got != nil {
		t.Fatalf("dead child's jobs = %+v", got)
	}
	es = append(es, history.Entry{Kind: "job", Data: map[string]any{"event": "finished", "id": float64(1)}})
	if got := RunningJobs(es, true); len(got) != 0 {
		t.Fatalf("typed records did not override legacy: %+v", got)
	}
}

func TestLegacyJobStartCandidates(t *testing.T) {
	t.Parallel()
	for _, text := range []string{
		"", "job ", "no jobs", "prefix job 3 started in the background (limit): ignored",
		"job 3 started in the background (limit): first",
		"prefix\njob 3 started in the background (limit): middle\nsuffix",
		"prefix\njob 3 started in the background (limit): last",
		"prefix\r\njob 3 started in the background (limit): CRLF\r\n",
		"prefix\rjob 3 started in the background (limit): ignored",
		"job 3 started in the background (multi\nline): accepted",
		"job 3 started in the background (limit): unicode λ😀",
		"\xff\njob 3 started in the background (limit): invalid UTF8",
	} {
		t.Run(fmt.Sprintf("%q", text), func(t *testing.T) {
			t.Parallel()
			at := time.Unix(1, 0)
			var want []Job
			for _, m := range jobStarted.FindAllStringSubmatch(text, -1) {
				want = append(want, Job{ID: 3, Cmd: m[2], Started: at})
			}
			got := RunningJobs([]history.Entry{{At: at, Kind: "result", Data: map[string]any{"text": text}}}, true)
			if len(got) != len(want) || len(want) > 0 && !reflect.DeepEqual(got, want) {
				t.Fatalf("jobs = %+v, want %+v", got, want)
			}
		})
	}
}

func BenchmarkRunningJobsLargeLegacyOutput(b *testing.B) {
	for _, tc := range []struct{ name, text string }{
		{"plain", strings.Repeat("deterministic tool output ", 32768)},
		{"lines", strings.Repeat("ordinary tool output\n", 40000)},
		{"inline_jobs", strings.Repeat("test output: job 99 started in the background (limit): not a notification\n", 12000)},
	} {
		b.Run(tc.name, func(b *testing.B) {
			es := []history.Entry{{Kind: "result", Data: map[string]any{"text": tc.text}}}
			b.SetBytes(int64(len(tc.text)))
			b.ReportAllocs()
			for b.Loop() {
				if got := RunningJobs(es, true); len(got) != 0 {
					b.Fatal(got)
				}
			}
		})
	}
}

func TestAPIListLegacyOutputMetadata(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now().UTC().Truncate(time.Second)
	es := []history.Entry{
		{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home, "repo": "/work", "branch": "topic", "origin": "web"}},
		{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "expanded input", "typed": "original prompt"}},
		{Seq: 3, At: now, Kind: "assistant", Data: map[string]any{"text": "checking", "model": "offline"}},
		{Seq: 4, At: now, Kind: "code", Data: map[string]any{"text": "tools.bash(\"go test ./...\")"}},
		{Seq: 5, At: now, Kind: "result", Data: map[string]any{"text": strings.Repeat("ordinary tool output\n", 40000), "exit": 1}},
		{Seq: 6, At: now, Kind: "done", Data: map[string]any{"usage": map[string]any{"cache_read": 10, "cache_write": 5, "in": 100}}},
		{Seq: 7, At: now, Kind: "turn-summary", Data: map[string]any{"text": "Tests need attention"}},
		{Seq: 8, At: now, Kind: "title", Data: map[string]any{"text": "Named session", "summary": "Session summary"}},
	}
	f.seed(t, "legacy", es...)
	var first map[string]any
	for i := range 2 {
		code, body := f.do(t, "GET", "/api/sessions", "")
		if code != 200 {
			t.Fatalf("list = %d, %v", code, body)
		}
		rows, ok := body["sessions"].([]any)
		if !ok || len(rows) != 1 {
			t.Fatalf("sessions = %v", body)
		}
		row := rows[0].(map[string]any)
		for key, want := range map[string]any{
			"id": "legacy", "title": "Named session", "summary": "Session summary", "outcome": "Tests need attention",
			"cwd": f.home, "repo": "/work", "branch": "topic", "status": "done", "model": "offline",
			"entries": float64(len(es)), "turns": float64(1), "testsFailed": true, "trouble": "tests failed",
		} {
			if got := row[key]; got != want {
				t.Errorf("read %d: %s = %v, want %v", i, key, got, want)
			}
		}
		cache, ok := row["cache"].(map[string]any)
		if !ok || cache["read"] != float64(10) || cache["write"] != float64(5) {
			t.Errorf("cache = %v", row["cache"])
		}
		if i == 0 {
			first = row
		} else if !reflect.DeepEqual(row, first) {
			t.Fatalf("warm row changed: got %v, want %v", row, first)
		}
	}
	f.seed(t, "legacy", history.Entry{Seq: 9, At: now, Kind: "title", Data: map[string]any{"text": "Renamed session", "summary": "Updated summary"}})
	code, body := f.do(t, "GET", "/api/sessions", "")
	if code != 200 {
		t.Fatalf("changed list = %d, %v", code, body)
	}
	row := body["sessions"].([]any)[0].(map[string]any)
	if row["title"] != "Renamed session" || row["summary"] != "Updated summary" || row["entries"] != float64(9) {
		t.Fatalf("stale metadata after append: %v", row)
	}
}
