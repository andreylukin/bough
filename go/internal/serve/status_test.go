package serve

import (
	"reflect"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// ent builds one history entry. Seq is 1-based and assigned by the
// caller's position so a table case reads like the file on disk.
func ent(seq int64, kind string, data map[string]any) history.Entry {
	return history.Entry{Seq: seq, At: time.Unix(1700000000+seq, 0).UTC(), Kind: kind, Data: data}
}

func entries(kinds ...history.Entry) []history.Entry { return kinds }

func text(s string) map[string]any { return map[string]any{"text": s} }

func TestStatusOf(t *testing.T) {
	t.Parallel()

	ask := func(seq int64, id string) history.Entry {
		return ent(seq, "ask", map[string]any{"id": id, "question": "which?", "options": []any{"a", "b"}})
	}

	cases := []struct {
		name    string
		entries []history.Entry
		alive   bool
		want    Status
		wantAsk *Ask
	}{
		{name: "no entries", want: StatusIdle},
		{
			name:    "only meta is still idle",
			entries: entries(ent(1, "meta", map[string]any{"cwd": "/tmp"})),
			want:    StatusIdle,
		},
		{
			name:    "open turn with a live child is running",
			entries: entries(ent(1, "input", text("hi")), ent(2, "assistant", text("working"))),
			alive:   true,
			want:    StatusRunning,
		},
		{
			name:    "open turn with no child is interrupted",
			entries: entries(ent(1, "input", text("hi")), ent(2, "assistant", text("working"))),
			want:    StatusInterrupted,
		},
		{
			name:    "closed turn is done",
			entries: entries(ent(1, "input", text("hi")), ent(2, "done", nil)),
			want:    StatusDone,
		},
		{
			name:    "cancelled turn is stopped",
			entries: entries(ent(1, "input", text("hi")), ent(2, "cancelled", map[string]any{"interrupted": true})),
			want:    StatusStopped,
		},
		{
			// The load-bearing case: there is no turn-level "error"
			// kind, so an errored turn still closes with "done".
			name: "an error entry inside a closed turn is error",
			entries: entries(
				ent(1, "input", text("hi")),
				ent(2, "error", text("boom")),
				ent(3, "done", nil),
			),
			want: StatusError,
		},
		{
			name: "a later clean turn clears the error",
			entries: entries(
				ent(1, "input", text("hi")),
				ent(2, "error", text("boom")),
				ent(3, "done", nil),
				ent(4, "input", text("again")),
				ent(5, "done", nil),
			),
			want: StatusDone,
		},
		{
			name: "error outranks a cancelled close",
			entries: entries(
				ent(1, "input", text("hi")),
				ent(2, "error", text("boom")),
				ent(3, "cancelled", nil),
			),
			want: StatusError,
		},
		{
			name:    "an unanswered ask outranks running",
			entries: entries(ent(1, "input", text("hi")), ask(2, "q1")),
			alive:   true,
			want:    StatusNeedsYou,
			wantAsk: &Ask{ID: "q1", Text: "which?", Options: []string{"a", "b"}, Seq: 2},
		},
		{
			name: "an answered ask goes back to running",
			entries: entries(
				ent(1, "input", text("hi")),
				ask(2, "q1"),
				ent(3, "ask/answer", map[string]any{"id": "q1", "text": "a"}),
			),
			alive: true,
			want:  StatusRunning,
		},
		{
			name: "an answer for another ask does not clear this one",
			entries: entries(
				ent(1, "input", text("hi")),
				ask(2, "q1"),
				ent(3, "ask/answer", map[string]any{"id": "other", "text": "a"}),
			),
			alive:   true,
			want:    StatusNeedsYou,
			wantAsk: &Ask{ID: "q1", Text: "which?", Options: []string{"a", "b"}, Seq: 2},
		},
		{
			name: "a closed turn cannot still be waiting on you",
			entries: entries(
				ent(1, "input", text("hi")),
				ask(2, "q1"),
				ent(3, "done", nil),
			),
			want: StatusDone,
		},
		{
			name: "a second turn's ask is what you are asked",
			entries: entries(
				ent(1, "input", text("one")),
				ent(2, "done", nil),
				ent(3, "input", text("two")),
				ask(4, "q2"),
			),
			alive:   true,
			want:    StatusNeedsYou,
			wantAsk: &Ask{ID: "q2", Text: "which?", Options: []string{"a", "b"}, Seq: 4},
		},
		{
			// History is hand-editable and can be truncated; a status
			// read must never panic on it.
			name:    "an ask with no data does not panic",
			entries: entries(ent(1, "input", text("hi")), ent(2, "ask", nil)),
			alive:   true,
			want:    StatusNeedsYou,
			wantAsk: &Ask{Seq: 2},
		},
		{
			name: "wrong-typed ask fields are ignored, not fatal",
			entries: entries(
				ent(1, "input", text("hi")),
				ent(2, "ask", map[string]any{"id": 7, "question": nil, "options": "a,b", "text": "fallback"}),
			),
			alive:   true,
			want:    StatusNeedsYou,
			wantAsk: &Ask{Text: "fallback", Seq: 2},
		},
		{
			name:    "entries with no turn at all are idle",
			entries: entries(ent(1, "usage", map[string]any{"tokens": 3})),
			want:    StatusIdle,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, ask := StatusOf(tc.entries, tc.alive)
			if got != tc.want {
				t.Errorf("status = %q, want %q", got, tc.want)
			}
			if tc.wantAsk == nil {
				if ask != nil {
					t.Errorf("ask = %+v, want nil", ask)
				}
				return
			}
			if ask == nil {
				t.Fatalf("ask = nil, want %+v", tc.wantAsk)
			}
			if !reflect.DeepEqual(ask, tc.wantAsk) {
				t.Errorf("ask = %+v, want %+v", ask, tc.wantAsk)
			}
		})
	}
}

func TestStatusOfAskOnlyForNeedsYou(t *testing.T) {
	t.Parallel()
	for _, st := range []struct {
		name string
		es   []history.Entry
	}{
		{"done", entries(ent(1, "input", text("x")), ent(2, "done", nil))},
		{"running", entries(ent(1, "input", text("x")))},
	} {
		if _, ask := StatusOf(st.es, true); ask != nil {
			t.Errorf("%s: ask = %+v, want nil", st.name, ask)
		}
	}
}

func TestTranscript(t *testing.T) {
	t.Parallel()
	es := entries(
		ent(1, "meta", map[string]any{"cwd": "/tmp"}),
		ent(2, "input", text("hi")),
		ent(3, "code", map[string]any{"code": "1+1", "text": "2"}),
		ent(4, "done", nil),
	)

	all := Transcript(es, 0, 0)
	if len(all) != 4 {
		t.Fatalf("len = %d, want 4", len(all))
	}
	if all[0].Data["cwd"] != "/tmp" {
		t.Errorf("meta data = %v, want cwd", all[0].Data)
	}
	if all[1].Text != "hi" {
		t.Errorf("input text = %q", all[1].Text)
	}
	// EntryText folds code in front of text; Data must not repeat it.
	if all[2].Text != "1+1\n2" {
		t.Errorf("code text = %q, want %q", all[2].Text, "1+1\n2")
	}
	if _, ok := all[2].Data["text"]; ok {
		t.Errorf("Data still carries text: %v", all[2].Data)
	}
	if all[2].Data["code"] != "1+1" {
		t.Errorf("Data lost code: %v", all[2].Data)
	}
	if all[3].Data != nil {
		t.Errorf("empty data = %v, want nil", all[3].Data)
	}

	if got := Transcript(es, 2, 0); len(got) != 2 || got[0].Seq != 3 {
		t.Errorf("since=2 = %+v, want seqs 3,4", got)
	}
	if got := Transcript(es, 0, 2); len(got) != 2 || got[1].Seq != 2 {
		t.Errorf("limit=2 = %+v, want seqs 1,2", got)
	}
	if got := Transcript(es, 99, 0); got != nil {
		t.Errorf("since past the end = %+v, want nil", got)
	}
}

func TestTranscriptCopiesData(t *testing.T) {
	t.Parallel()
	e := ent(1, "input", map[string]any{"text": "hi", "tag": "a"})
	got := Transcript([]history.Entry{e}, 0, 0)
	got[0].Data["tag"] = "mutated"
	if e.Data["tag"] != "a" {
		t.Errorf("Transcript aliased the entry's Data: %v", e.Data)
	}
}

func TestLastActivity(t *testing.T) {
	t.Parallel()
	if got := LastActivity(nil); !got.IsZero() {
		t.Errorf("empty = %v, want zero", got)
	}
	es := entries(ent(1, "input", text("a")), ent(2, "done", nil))
	if got := LastActivity(es); !got.Equal(es[1].At) {
		t.Errorf("last = %v, want %v", got, es[1].At)
	}
}
