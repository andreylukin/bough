package wordfreq

import (
	"reflect"
	"testing"
)

func TestTopNTiesAreAlphabetical(t *testing.T) {
	got := TopN("pear apple fig apple pear fig kiwi", 3)
	want := []Count{{"apple", 2}, {"fig", 2}, {"pear", 2}}
	for i := 0; i < 20; i++ { // map order is random; one lucky run proves nothing
		if got = TopN("pear apple fig apple pear fig kiwi", 3); !reflect.DeepEqual(got, want) {
			t.Fatalf("TopN = %v, want %v", got, want)
		}
	}
}

func TestTopNMoreThanDistinct(t *testing.T) {
	got := TopN("go go gopher", 5)
	want := []Count{{"go", 2}, {"gopher", 1}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("TopN = %v, want %v", got, want)
	}
}
