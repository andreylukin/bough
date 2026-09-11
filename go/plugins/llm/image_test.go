package llm

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestImageRefs(t *testing.T) {
	got := ImageRefs("look [Image #1: /a/b.png] and [Image: /c.jpg] not [Image #2]")
	if len(got) != 2 || got[0] != "/a/b.png" || got[1] != "/c.jpg" {
		t.Fatalf("refs = %q", got)
	}
}

// Every vision provider sends the referenced image as pixels.
func TestProvidersSendImages(t *testing.T) {
	img := filepath.Join(t.TempDir(), "x.png")
	if err := os.WriteFile(img, []byte("\x89PNG"), 0o644); err != nil {
		t.Fatal(err)
	}
	msgs := []Message{{Role: "user", Content: "see [Image #1: " + img + "]", Images: []string{img}}}
	const b64 = "iVBORw==" // base64 of "\x89PNG"

	a, _ := json.Marshal((&anthropicLLM{}).params("", msgs))
	if !strings.Contains(string(a), `"type":"image"`) || !strings.Contains(string(a), b64) {
		t.Errorf("anthropic: %s", a)
	}
	o, _ := json.Marshal((&openaiLLM{}).body("", msgs, false))
	if !strings.Contains(string(o), `"input_image"`) || !strings.Contains(string(o), "data:image/png;base64,"+b64) {
		t.Errorf("openai: %s", o)
	}
}
