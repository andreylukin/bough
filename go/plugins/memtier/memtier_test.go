package memtier

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// fakeNav answers pick calls with pickReply and index calls with
// "#SEQ: line" for every seq it is shown.
type fakeNav struct {
	pickReply string
	calls     []string
}

func (f *fakeNav) Complete(_ context.Context, system string, msgs []llm.Message) (string, error) {
	f.calls = append(f.calls, msgs[0].Content)
	if system == pickPrompt {
		return f.pickReply, nil
	}
	var b strings.Builder
	for line := range strings.SplitSeq(msgs[0].Content, "\n") {
		if rest, ok := strings.CutPrefix(line, "=== #"); ok {
			seq, _, _ := strings.Cut(rest, " ")
			b.WriteString("#" + seq + ": summary of " + seq + "\n")
		}
	}
	return b.String(), nil
}

func session(results int, size int) []history.Entry {
	var es []history.Entry
	seq := int64(0)
	next := func(kind, text string) {
		seq++
		es = append(es, history.Entry{Seq: seq, Kind: kind, Data: map[string]any{"text": text}})
	}
	next("input", "first prompt")
	for i := range results {
		next("assistant", "step")
		next("result", "output "+string(rune('a'+i))+"\n"+strings.Repeat("x", size))
	}
	next("input", "what did output c say?")
	return es
}

func TestUnderBudgetUntouched(t *testing.T) {
	tr := New(nil)
	es := session(10, 100)
	got := tr.Project(es)
	for _, m := range got {
		if strings.Contains(m.Content, "hidden") {
			t.Fatalf("hid an output under budget: %q", m.Content)
		}
	}
}

func TestOldOutputsHideNewestStay(t *testing.T) {
	tr := New(nil)
	tr.budget = 5_000
	tr.keepWhole = 2
	es := session(6, 1_500) // 9k of output, budget 5k
	got := tr.Project(es)
	hidden, whole := 0, 0
	for _, m := range got {
		if !strings.HasPrefix(m.Content, "[tool output]") {
			continue
		}
		if strings.Contains(m.Content, "hidden ·") {
			hidden++
			if !strings.Contains(m.Content, "output ") {
				t.Errorf("placeholder lost its first line: %q", m.Content)
			}
		} else {
			whole++
		}
	}
	if whole < 2 {
		t.Fatalf("newest results must stay whole, got %d", whole)
	}
	if hidden == 0 {
		t.Fatal("nothing hidden over budget")
	}
	// The newest two (seqs 11 and 13) are never hidden.
	for _, m := range got {
		if strings.Contains(m.Content, "[#11 hidden") || strings.Contains(m.Content, "[#13 hidden") {
			t.Fatalf("hid a protected result: %q", m.Content)
		}
	}
}

func TestDeclaredFocusComesBack(t *testing.T) {
	tr := New(nil)
	tr.budget = 5_000
	tr.keepWhole = 1
	es := session(6, 1_500)
	// Seq 5 is the third result ("output c"). The model declares it
	// in the current turn.
	es = append(es, history.Entry{Seq: 100, Kind: "assistant", Data: map[string]any{"text": "I need that.\n<focus seq=5>\n```js\n1\n```"}})
	got := tr.Project(es)
	for _, m := range got {
		if strings.Contains(m.Content, "[#5 hidden") {
			t.Fatal("declared focus was still hidden")
		}
	}
	if len(ParseFocus("<focus seq=\"5, 7\"/> and <focus seq=5>")) != 2 {
		t.Fatal("ParseFocus should dedupe and read both forms")
	}
}

func TestNavigatorPickAndIndex(t *testing.T) {
	nav := &fakeNav{pickReply: "#7, 3"}
	tr := New(func() llm.LLM { return nav })
	tr.budget = 5_000
	tr.keepWhole = 1
	es := session(6, 1_500)
	tr.Index(es)
	if tr.index[7] != "summary of 7" {
		t.Fatalf("index line not recorded: %v", tr.index)
	}
	got := tr.Project(es)
	for _, m := range got {
		if strings.Contains(m.Content, "[#7 hidden") || strings.Contains(m.Content, "[#3 hidden") {
			t.Fatalf("picked output still hidden: %q", m.Content)
		}
		if strings.Contains(m.Content, "[#5 hidden") && !strings.Contains(m.Content, "summary of 5") {
			t.Fatalf("placeholder should carry the index line: %q", m.Content)
		}
	}
	picks := 0
	for _, c := range nav.calls {
		if strings.HasPrefix(c, "Request:") {
			picks++
		}
	}
	tr.Project(es)
	if picks != 1 || len(nav.calls) != picks+1 {
		t.Fatalf("pick must run once per turn: %d calls", len(nav.calls))
	}
}

func TestParseIndex(t *testing.T) {
	m := ParseIndex("#3: ls of repo, 12 files\n#9:  bq query, 312 rows\nnoise\n#x: bad")
	if m[3] != "ls of repo, 12 files" || m[9] != "bq query, 312 rows" || len(m) != 2 {
		t.Fatalf("got %v", m)
	}
}

// fakeMemoryd records what it was given to index and answers recall,
// note and search from those chunks by substring.
func fakeMemoryd(t *testing.T) (*httptest.Server, *[]map[string]any) {
	t.Helper()
	var fed []map[string]any
	var mu sync.Mutex
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var req map[string]any
		_ = json.NewDecoder(r.Body).Decode(&req)
		mu.Lock()
		defer mu.Unlock()
		enc := json.NewEncoder(w)
		switch r.URL.Path {
		case "/index":
			fed = append(fed, req)
			_ = enc.Encode(map[string]any{"line": "line for " + fmt.Sprint(req["seq"])})
		case "/search":
			q, _ := req["query"].(string)
			var hits []map[string]any
			for _, c := range fed {
				if strings.Contains(c["text"].(string), q) {
					hits = append(hits, map[string]any{"session": c["session"], "seq": c["seq"], "kind": c["kind"], "line": "l"})
				}
			}
			_ = enc.Encode(map[string]any{"hits": hits})
		case "/recall":
			q, _ := req["question"].(string)
			for _, c := range fed {
				if strings.Contains(c["text"].(string), q) {
					_ = enc.Encode(map[string]any{"answer": "V-" + q, "seq": c["seq"], "session": c["session"], "quote": q, "verified": true})
					return
				}
			}
			_ = enc.Encode(map[string]any{"answer": nil, "verified": false, "raw": "guess"})
		case "/note":
			q, _ := req["request"].(string)
			if strings.Contains(q, "nothing-here") {
				_ = enc.Encode(map[string]any{"facts": []any{}})
				return
			}
			_ = enc.Encode(map[string]any{"facts": []map[string]any{{"seq": 3, "session": "s1", "quote": "output a", "fact": "output a was printed"}}})
		case "/consolidate":
			fed = append(fed, map[string]any{"session": "ledger", "seq": float64(0), "kind": "ledger", "text": fmt.Sprintf("consolidated %v-%v", req["from_seq"], req["to_seq"])})
			_ = enc.Encode(map[string]any{"records": 1})
		}
	}))
	t.Cleanup(srv.Close)
	return srv, &fed
}

type memHist struct{ es []history.Entry }

func (m memHist) Entries() []history.Entry { return m.es }

func TestFeederNoteRecallPick(t *testing.T) {
	srv, fed := fakeMemoryd(t)
	c := newMemoryClient(srv.URL, "s1")
	es := session(3, 100) // seqs 1..8: input, (assistant, result)x3, input
	f := &feeder{c: c, hist: memHist{es}, ctx: t.Context(), fail: func(err error) { t.Error(err) }}
	f.TurnDone(7)
	f.Wait(5 * time.Second)
	if len(*fed) != 9 || (*fed)[0]["kind"] != "user" || (*fed)[2]["kind"] != "tool output" {
		t.Fatalf("fed %d entries: %v", len(*fed), *fed)
	}
	if last := (*fed)[8]; last["kind"] != "ledger" || !strings.Contains(last["text"].(string), "consolidated 1-7") {
		t.Fatalf("turn not consolidated after feeding: %v", last)
	}

	tr := New(nil)
	tr.mem = c
	got := tr.Project(es)
	last := got[len(got)-1].Content
	if !strings.Contains(last, "[memory, from the local model") || !strings.Contains(last, "output a was printed (from #3") {
		t.Fatalf("note missing from the request: %q", last)
	}
	tr.Project(es)
	if n := len(tr.notes); n != 1 {
		t.Fatalf("note must be asked once per turn, got %d", n)
	}
	es2 := append(slices.Clone(es), history.Entry{Seq: 99, Kind: "input", Data: map[string]any{"text": "nothing-here please"}})
	if got = tr.Project(es2); strings.Contains(got[len(got)-1].Content, "[memory") {
		t.Fatal("an empty note must leave the request untouched")
	}
	if a, err := tr.recall("output b"); err != nil || !strings.HasPrefix(a, "V-output b") || !strings.Contains(a, "verified in #5") {
		t.Fatalf("recall: %q %v", a, err)
	}
	if a, _ := tr.recall("absent"); !strings.HasPrefix(a, "not in memory") || !strings.Contains(a, "guess") {
		t.Fatalf("unverified recall must say so: %q", a)
	}
	// The pick comes from the index: a prompt naming "output a" brings
	// seq 3 back even though it is the oldest.
	tr.budget = 100
	tr.keepWhole = 1
	es3 := append(slices.Clone(es), history.Entry{Seq: 100, Kind: "input", Data: map[string]any{"text": "output a"}})
	for _, m := range tr.Project(es3) {
		if strings.Contains(m.Content, "[#3 hidden") {
			t.Fatal("index pick should have kept #3 in full")
		}
	}
}

func TestRecallOutputIsNotEvidence(t *testing.T) {
	srv, fed := fakeMemoryd(t)
	c := newMemoryClient(srv.URL, "s1")
	es := []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "ask memory"}},
		{Seq: 2, Kind: "code", Data: map[string]any{"text": "console.log(tools.recall('x'))"}},
		{Seq: 3, Kind: "result", Data: map[string]any{"text": "V-x (verified in #9)"}},
		{Seq: 4, Kind: "code", Data: map[string]any{"text": "console.log(tools.bash('ls'))"}},
		{Seq: 5, Kind: "result", Data: map[string]any{"text": "a.txt"}},
	}
	f := &feeder{c: c, hist: memHist{es}, ctx: t.Context(), fail: func(err error) { t.Error(err) }}
	f.Kick()
	f.Wait(5 * time.Second)
	if (*fed)[2]["kind"] != "recall" || (*fed)[4]["kind"] != "tool output" {
		t.Fatalf("recall result must be tagged, bash result not: %v %v", (*fed)[2]["kind"], (*fed)[4]["kind"])
	}
}
