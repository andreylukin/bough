// Package schema is the small JSON Schema subset bough validates a
// structured answer against: enough to pin the SHAPE of a stop block
// or a subagent's report, not a general validator.
//
// bough's completion is text, not a tool call, so a provider cannot
// constrain the decoding for us the way strict function calling does.
// The equivalent is to check what came back and hand the model its own
// mistakes: Validate returns issues in the model's words ("findings:
// expected array, got string"), the loop feeds them back, and the turn
// ends only on a valid answer or a spent retry budget — never with a
// silently malformed result.
//
// Supported: type (string, number, integer, boolean, object, array,
// null, or a list of those), properties, required, items, enum,
// additionalProperties: false, minItems/maxItems, minimum/maximum.
// Anything else in the schema is ignored rather than rejected, so a
// richer schema still pins what this understands.
package schema

import (
	"encoding/json"
	"fmt"
	"math"
	"sort"
	"strings"
)

// Schema is a decoded JSON Schema object.
type Schema map[string]any

// Parse reads a schema from JSON.
func Parse(b []byte) (Schema, error) {
	var s Schema
	if err := json.Unmarshal(b, &s); err != nil {
		return nil, fmt.Errorf("schema: %w", err)
	}
	return s, nil
}

// ValidateJSON parses body as JSON and validates it, returning the
// decoded value and the issues. A body that is not JSON at all is one
// issue and a nil value.
func (s Schema) ValidateJSON(body string) (any, []string) {
	body = strings.TrimSpace(body)
	// A model likes to wrap JSON in a fence even inside a stop block.
	if inner, ok := strings.CutPrefix(body, "```"); ok {
		if _, rest, cut := strings.Cut(inner, "\n"); cut {
			body = strings.TrimSuffix(strings.TrimSpace(rest), "```")
		}
	}
	var v any
	if err := json.Unmarshal([]byte(strings.TrimSpace(body)), &v); err != nil {
		return nil, []string{"the answer is not JSON: " + err.Error()}
	}
	return v, s.Validate(v)
}

// Validate reports every way v fails s, deepest path first, in words a
// model can act on. Empty means valid.
func (s Schema) Validate(v any) []string {
	var out []string
	s.check("", v, &out)
	sort.Strings(out)
	return out
}

func (s Schema) check(path string, v any, out *[]string) {
	if len(s) == 0 {
		return
	}
	if t, ok := s["type"]; ok && !typeOK(t, v) {
		*out = append(*out, at(path)+"expected "+typeName(t)+", got "+kind(v))
		return // a wrong type makes every nested complaint noise
	}
	if e, ok := s["enum"].([]any); ok && !contains(e, v) {
		*out = append(*out, at(path)+"must be one of "+list(e)+", got "+render(v))
	}
	switch val := v.(type) {
	case map[string]any:
		s.checkObject(path, val, out)
	case []any:
		s.checkArray(path, val, out)
	case float64:
		if m, ok := num(s["minimum"]); ok && val < m {
			*out = append(*out, at(path)+fmt.Sprintf("must be >= %v, got %v", m, val))
		}
		if m, ok := num(s["maximum"]); ok && val > m {
			*out = append(*out, at(path)+fmt.Sprintf("must be <= %v, got %v", m, val))
		}
	}
}

func (s Schema) checkObject(path string, obj map[string]any, out *[]string) {
	props, _ := s["properties"].(map[string]any)
	for _, r := range strs(s["required"]) {
		if _, ok := obj[r]; !ok {
			*out = append(*out, at(path)+"missing required field "+r)
		}
	}
	if add, ok := s["additionalProperties"].(bool); ok && !add {
		for k := range obj {
			if _, known := props[k]; !known {
				*out = append(*out, at(path)+"unexpected field "+k)
			}
		}
	}
	for k, sub := range props {
		v, ok := obj[k]
		if !ok {
			continue // absence is required's business
		}
		child, _ := sub.(map[string]any)
		Schema(child).check(join(path, k), v, out)
	}
}

func (s Schema) checkArray(path string, arr []any, out *[]string) {
	if n, ok := num(s["minItems"]); ok && float64(len(arr)) < n {
		*out = append(*out, at(path)+fmt.Sprintf("needs at least %v item(s), got %d", n, len(arr)))
	}
	if n, ok := num(s["maxItems"]); ok && float64(len(arr)) > n {
		*out = append(*out, at(path)+fmt.Sprintf("takes at most %v item(s), got %d", n, len(arr)))
	}
	items, _ := s["items"].(map[string]any)
	if len(items) == 0 {
		return
	}
	for i, v := range arr {
		Schema(items).check(fmt.Sprintf("%s[%d]", path, i), v, out)
	}
}

// typeOK reports whether v matches a type keyword (a string or a list).
func typeOK(t, v any) bool {
	switch tt := t.(type) {
	case string:
		return oneType(tt, v)
	case []any:
		for _, x := range tt {
			if s, ok := x.(string); ok && oneType(s, v) {
				return true
			}
		}
		return false
	}
	return true // an unreadable type keyword constrains nothing
}

func oneType(t string, v any) bool {
	switch t {
	case "object":
		_, ok := v.(map[string]any)
		return ok
	case "array":
		_, ok := v.([]any)
		return ok
	case "string":
		_, ok := v.(string)
		return ok
	case "boolean":
		_, ok := v.(bool)
		return ok
	case "null":
		return v == nil
	case "number":
		_, ok := v.(float64)
		return ok
	case "integer":
		f, ok := v.(float64)
		return ok && f == math.Trunc(f)
	}
	return true
}

func typeName(t any) string {
	switch tt := t.(type) {
	case string:
		return tt
	case []any:
		var names []string
		for _, x := range tt {
			names = append(names, fmt.Sprint(x))
		}
		return strings.Join(names, " or ")
	}
	return fmt.Sprint(t)
}

// kind names what actually arrived, in JSON words.
func kind(v any) string {
	switch v.(type) {
	case nil:
		return "null"
	case bool:
		return "boolean"
	case float64:
		return "number"
	case string:
		return "string"
	case []any:
		return "array"
	case map[string]any:
		return "object"
	}
	return fmt.Sprintf("%T", v)
}

func at(path string) string {
	if path == "" {
		return ""
	}
	return path + ": "
}

func join(path, key string) string {
	if path == "" {
		return key
	}
	return path + "." + key
}

func strs(v any) []string {
	list, _ := v.([]any)
	out := make([]string, 0, len(list))
	for _, x := range list {
		if s, ok := x.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func num(v any) (float64, bool) {
	f, ok := v.(float64)
	return f, ok
}

func contains(list []any, v any) bool {
	for _, x := range list {
		if fmt.Sprint(x) == fmt.Sprint(v) {
			return true
		}
	}
	return false
}

func list(vals []any) string {
	var out []string
	for _, v := range vals {
		out = append(out, render(v))
	}
	return strings.Join(out, ", ")
}

func render(v any) string {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Sprint(v)
	}
	return string(b)
}

// Describe renders the schema for a prompt: the JSON itself, which is
// what a model follows best, trimmed of indentation noise.
func (s Schema) Describe() string {
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return ""
	}
	return string(b)
}
