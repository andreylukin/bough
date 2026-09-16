package skills

import "testing"

func TestDescriptionBlockScalar(t *testing.T) {
	t.Parallel()
	for in, want := range map[string]string{
		"---\ndescription: |\n  Does X\n  more\n---\n": "Does X",
		"---\ndescription: >-\n\n  Does X.\nname: a\n": "Does X",
		"---\ndescription: |\nname: a\n---\n":          "",
		"---\ndescription: \"Does Y\"\n":               "Does Y",
	} {
		if got := summarize(descriptionOf(in)); got != want {
			t.Errorf("%q: got %q, want %q", in, got, want)
		}
	}
}
