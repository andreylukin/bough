package schema

import (
	"strings"
	"testing"
)

var findings = Schema{
	"type":     "object",
	"required": []any{"status", "findings"},
	"properties": map[string]any{
		"status":   map[string]any{"type": "string", "enum": []any{"ok", "failed"}},
		"findings": map[string]any{"type": "array", "items": map[string]any{"type": "string"}, "minItems": 1.0},
		"files":    map[string]any{"type": "array", "items": map[string]any{"type": "string"}},
		"steps":    map[string]any{"type": "integer", "minimum": 0.0},
	},
	"additionalProperties": false,
}

func TestValidAnswerPasses(t *testing.T) {
	v, issues := findings.ValidateJSON(`{"status":"ok","findings":["the golden file was stale"],"steps":3}`)
	if len(issues) != 0 {
		t.Fatalf("issues on a valid answer: %v", issues)
	}
	if m, ok := v.(map[string]any); !ok || m["status"] != "ok" {
		t.Fatalf("decoded = %#v", v)
	}
}

// Every issue names the field and what was wrong, in words the model
// can act on — that text is what gets fed back to it.
func TestIssuesNameTheField(t *testing.T) {
	cases := []struct{ name, body, want string }{
		{"missing", `{"status":"ok"}`, "missing required field findings"},
		{"wrong type", `{"status":"ok","findings":"just one"}`, "findings: expected array, got string"},
		{"item type", `{"status":"ok","findings":[1]}`, "findings[0]: expected string, got number"},
		{"enum", `{"status":"done","findings":["x"]}`, `status: must be one of "ok", "failed", got "done"`},
		{"extra", `{"status":"ok","findings":["x"],"mood":"great"}`, "unexpected field mood"},
		{"empty array", `{"status":"ok","findings":[]}`, "findings: needs at least 1 item(s), got 0"},
		{"not integer", `{"status":"ok","findings":["x"],"steps":1.5}`, "steps: expected integer, got number"},
		{"below minimum", `{"status":"ok","findings":["x"],"steps":-1}`, "steps: must be >= 0, got -1"},
		{"not an object", `["a"]`, "expected object, got array"},
		{"not json", `Status: ok, findings: none`, "the answer is not JSON"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, issues := findings.ValidateJSON(c.body)
			if len(issues) == 0 {
				t.Fatalf("no issue for %s", c.body)
			}
			if !strings.Contains(strings.Join(issues, " | "), c.want) {
				t.Fatalf("issues %v do not mention %q", issues, c.want)
			}
		})
	}
}

// A model fences its JSON even inside a stop block; that is not a
// schema violation.
func TestFencedJSONIsAccepted(t *testing.T) {
	for _, body := range []string{
		"```json\n{\"status\":\"ok\",\"findings\":[\"x\"]}\n```",
		"```\n{\"status\":\"ok\",\"findings\":[\"x\"]}\n```",
		"  {\"status\":\"ok\",\"findings\":[\"x\"]}  ",
	} {
		if _, issues := findings.ValidateJSON(body); len(issues) != 0 {
			t.Fatalf("%q -> %v", body, issues)
		}
	}
}

// A wrong type stops the walk into that value: one complaint, not one
// per field the model never wrote.
func TestWrongTypeDoesNotCascade(t *testing.T) {
	_, issues := findings.ValidateJSON(`"a plain string"`)
	if len(issues) != 1 {
		t.Fatalf("want one issue, got %v", issues)
	}
}

// An empty schema constrains nothing; unknown keywords are ignored
// rather than rejected.
func TestEmptyAndUnknownKeywords(t *testing.T) {
	if issues := (Schema{}).Validate(map[string]any{"anything": true}); len(issues) != 0 {
		t.Fatalf("empty schema complained: %v", issues)
	}
	s := Schema{"type": "object", "patternProperties": map[string]any{"^x": map[string]any{}}}
	if issues := s.Validate(map[string]any{"xy": 1.0}); len(issues) != 0 {
		t.Fatalf("unknown keyword rejected: %v", issues)
	}
}

func TestDescribeIsTheSchemaItself(t *testing.T) {
	got := findings.Describe()
	for _, want := range []string{`"required"`, `"findings"`, `"enum"`} {
		if !strings.Contains(got, want) {
			t.Fatalf("Describe missing %q:\n%s", want, got)
		}
	}
}
