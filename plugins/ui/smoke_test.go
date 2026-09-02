package ui

import "testing"

// Compile-time smoke: eventOf normalizes the payload shapes loop may emit.
func TestEventOf(t *testing.T) {
	type loopEvent struct{ Kind, Text string }
	cases := []struct {
		payload any
		want    Event
	}{
		{Event{"assistant", "hi"}, Event{"assistant", "hi"}},
		{loopEvent{"code", "1+1"}, Event{"code", "1+1"}},
		{&loopEvent{"result", "2"}, Event{"result", "2"}},
		{map[string]any{"Kind": "error", "Text": "boom"}, Event{"error", "boom"}},
		{map[string]string{"Kind": "done", "Text": ""}, Event{"done", ""}},
	}
	for _, c := range cases {
		if got := eventOf(c.payload); got != c.want {
			t.Errorf("eventOf(%#v) = %#v, want %#v", c.payload, got, c.want)
		}
	}
}
