package ui

import "testing"

// Compile-time smoke: eventOf normalizes the payload shapes loop may emit.
func TestEventOf(t *testing.T) {
	type loopEvent struct{ Kind, Text string }
	cases := []struct {
		payload any
		want    Event
	}{
		{Event{Kind: "assistant", Text: "hi"}, Event{Kind: "assistant", Text: "hi"}},
		{loopEvent{"code", "1+1"}, Event{Kind: "code", Text: "1+1"}},
		{&loopEvent{"result", "2"}, Event{Kind: "result", Text: "2"}},
		{map[string]any{"Kind": "error", "Text": "boom"}, Event{Kind: "error", Text: "boom"}},
		{map[string]string{"Kind": "done", "Text": ""}, Event{Kind: "done"}},
	}
	for _, c := range cases {
		if got := eventOf(c.payload); got.Kind != c.want.Kind || got.Text != c.want.Text {
			t.Errorf("eventOf(%#v) = %#v, want %#v", c.payload, got, c.want)
		}
	}
}
