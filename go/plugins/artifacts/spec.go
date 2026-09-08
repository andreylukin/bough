package artifacts

// The spec: a page is a title and a list of blocks; each block is a
// small typed object. The model writes the spec, this file checks it
// and says exactly what is wrong (block number, type, field), and the
// viewer draws it. Everything about how a page looks lives in the
// viewer, so the model spends its tokens on content only.

import (
	"errors"
	"fmt"
	"sort"
	"strings"
)

// Guide is the spec reference the model reads. It is prompt text, kept
// short: one line per block, the shorthand, and the rules a validator
// would otherwise have to explain after the fact.
const Guide = `A page is {title, subtitle?, blocks: [...]}. A block is an object with "type", or a plain string (= markdown text). Types:
- text {md}: markdown — paragraphs, **bold**, _em_, ` + "`code`" + `, [links](url), "- " lists.
- heading {text}: a section title (use "section" to group blocks under one).
- stats {items: [{label, value, delta?, tone?}]}: big-number tiles; tone good|warn|bad colours the delta.
- table {columns: [..], rows: [[..]..], align?: ["left"|"right"..], caption?}: sortable; numbers right-align themselves.
- chart {kind: bar|line|area|pie|scatter, x: [..], series: [{name, values: [..]}], title?, unit?, stacked?}: values line up with x (scatter: values are [x, y] pairs).
- list {items: [..], ordered?}
- kv {items: [{key, value}]}: facts as a definition list.
- callout {tone: info|good|warn|bad, title?, text}
- code {lang?, text}
- timeline {items: [{when, what, tone?}]}
- grid {columns: 2|3|4, blocks: [..]}: blocks side by side.
- section {title, blocks: [..], collapsed?}: a titled group.
Rules: vary block types (a page of look-alike tables reads as a dump); put the finding in stats or a callout before the evidence; never restate the page title in a heading; no process or model metadata in the page.`

// Block types and their required / optional fields.
var blockFields = map[string]struct{ required, optional []string }{
	"text":     {[]string{"md"}, nil},
	"heading":  {[]string{"text"}, nil},
	"stats":    {[]string{"items"}, nil},
	"table":    {[]string{"columns", "rows"}, []string{"align", "caption"}},
	"chart":    {[]string{"kind", "series"}, []string{"x", "title", "unit", "stacked"}},
	"list":     {[]string{"items"}, []string{"ordered"}},
	"kv":       {[]string{"items"}, nil},
	"callout":  {[]string{"text"}, []string{"tone", "title"}},
	"code":     {[]string{"text"}, []string{"lang"}},
	"timeline": {[]string{"items"}, nil},
	"grid":     {[]string{"blocks"}, []string{"columns"}},
	"section":  {[]string{"title", "blocks"}, []string{"collapsed"}},
}

var chartKinds = []string{"bar", "line", "area", "pie", "scatter"}
var tones = []string{"info", "good", "warn", "bad"}

// Normalize checks a page spec and returns it with shorthand expanded
// (a string block becomes a text block). The error lists every problem
// found, one per line, so a repair takes one round trip.
func Normalize(spec any) (map[string]any, error) {
	page, ok := spec.(map[string]any)
	if !ok {
		return nil, errors.New("the spec must be an object {title, blocks: [...]}")
	}
	var issues []string
	title, _ := page["title"].(string)
	if strings.TrimSpace(title) == "" {
		issues = append(issues, "title: required (a short name for the page)")
	}
	for k := range page {
		switch k {
		case "title", "subtitle", "blocks":
		default:
			issues = append(issues, fmt.Sprintf("%s: unknown page field (page fields are title, subtitle, blocks)", k))
		}
	}
	blocks, err := normalizeBlocks(page["blocks"], "blocks")
	if err != nil {
		issues = append(issues, err.Error())
	}
	if len(issues) > 0 {
		return nil, errors.New("artifact spec:\n  " + strings.Join(issues, "\n  "))
	}
	out := map[string]any{"title": title, "blocks": blocks}
	if s, _ := page["subtitle"].(string); s != "" {
		out["subtitle"] = s
	}
	return out, nil
}

// normalizeBlocks checks a block list; path names it in messages
// ("blocks", "blocks[3].blocks").
func normalizeBlocks(v any, path string) ([]any, error) {
	list, ok := v.([]any)
	if !ok {
		return nil, fmt.Errorf("%s: required, an array of blocks", path)
	}
	if len(list) == 0 {
		return nil, fmt.Errorf("%s: empty", path)
	}
	var issues []string
	out := make([]any, 0, len(list))
	for i, b := range list {
		at := fmt.Sprintf("%s[%d]", path, i)
		if s, ok := b.(string); ok {
			out = append(out, map[string]any{"type": "text", "md": s})
			continue
		}
		m, ok := b.(map[string]any)
		if !ok {
			issues = append(issues, at+": a block is an object with a type, or a string")
			continue
		}
		typ, _ := m["type"].(string)
		fields, known := blockFields[typ]
		if !known {
			issues = append(issues, fmt.Sprintf("%s: unknown type %q (types: %s)", at, typ, strings.Join(blockTypes(), ", ")))
			continue
		}
		at += " (" + typ + ")"
		for _, f := range fields.required {
			if _, ok := m[f]; !ok {
				issues = append(issues, fmt.Sprintf("%s: missing %s", at, f))
			}
		}
		for k := range m {
			if k != "type" && !contains(fields.required, k) && !contains(fields.optional, k) {
				issues = append(issues, fmt.Sprintf("%s: unknown field %s", at, k))
			}
		}
		if more := checkBlock(typ, m, at); len(more) > 0 {
			issues = append(issues, more...)
		}
		if typ == "grid" || typ == "section" {
			inner, err := normalizeBlocks(m["blocks"], at+".blocks")
			if err != nil {
				issues = append(issues, err.Error())
			} else {
				m["blocks"] = inner
			}
		}
		out = append(out, m)
	}
	if len(issues) > 0 {
		return nil, errors.New(strings.Join(issues, "\n  "))
	}
	return out, nil
}

// checkBlock is the per-type shape check beyond field presence.
func checkBlock(typ string, m map[string]any, at string) []string {
	var issues []string
	bad := func(format string, a ...any) { issues = append(issues, at+": "+fmt.Sprintf(format, a...)) }
	str := func(f string) (string, bool) { s, ok := m[f].(string); return s, ok }
	items := func(f string) []any {
		l, ok := m[f].([]any)
		if !ok {
			if _, present := m[f]; present {
				bad("%s must be an array", f)
			}
			return nil
		}
		if len(l) == 0 {
			bad("%s is empty", f)
		}
		return l
	}
	objects := func(f string, need ...string) {
		for i, it := range items(f) {
			o, ok := it.(map[string]any)
			if !ok {
				bad("%s[%d] must be an object with %s", f, i, strings.Join(need, ", "))
				continue
			}
			for _, k := range need {
				if _, ok := o[k]; !ok {
					bad("%s[%d]: missing %s", f, i, k)
				}
			}
			if t, ok := o["tone"].(string); ok && !contains(tones, t) {
				bad("%s[%d]: tone must be one of %s", f, i, strings.Join(tones, "|"))
			}
		}
	}
	switch typ {
	case "text", "heading", "code":
		f := map[string]string{"text": "md", "heading": "text", "code": "text"}[typ]
		if s, ok := str(f); ok && strings.TrimSpace(s) == "" {
			bad("%s is empty", f)
		} else if !ok && m[f] != nil {
			bad("%s must be a string", f)
		}
	case "stats":
		objects("items", "label", "value")
	case "kv":
		objects("items", "key", "value")
	case "timeline":
		objects("items", "when", "what")
	case "list":
		items("items")
	case "callout":
		if t, ok := str("tone"); ok && !contains(tones, t) {
			bad("tone must be one of %s", strings.Join(tones, "|"))
		}
	case "table":
		cols := items("columns")
		for i, r := range items("rows") {
			row, ok := r.([]any)
			if !ok {
				bad("rows[%d] must be an array of cells", i)
				continue
			}
			if len(cols) > 0 && len(row) != len(cols) {
				bad("rows[%d] has %d cells, %d columns", i, len(row), len(cols))
			}
		}
		if a, present := m["align"]; present {
			if al, ok := a.([]any); !ok || (len(cols) > 0 && len(al) != len(cols)) {
				bad("align must list one of left|right|center per column")
			}
		}
	case "chart":
		kind, _ := str("kind")
		if !contains(chartKinds, kind) {
			bad("kind must be one of %s", strings.Join(chartKinds, "|"))
		}
		x, _ := m["x"].([]any)
		if kind != "scatter" && len(x) == 0 {
			bad("x is required for %s charts (the categories or time points)", kind)
		}
		for i, s := range items("series") {
			o, ok := s.(map[string]any)
			if !ok {
				bad("series[%d] must be {name, values}", i)
				continue
			}
			vals, ok := o["values"].([]any)
			if !ok {
				bad("series[%d]: missing values", i)
				continue
			}
			if kind != "scatter" && len(x) > 0 && len(vals) != len(x) {
				bad("series[%d] has %d values, x has %d", i, len(vals), len(x))
			}
			if _, ok := o["name"]; !ok && len(items("series")) > 1 {
				bad("series[%d]: missing name", i)
			}
		}
	case "grid":
		if c, present := m["columns"]; present {
			if n, ok := c.(float64); !ok || n < 2 || n > 4 {
				bad("columns must be 2, 3 or 4")
			}
		}
	case "section":
		if s, ok := str("title"); ok && strings.TrimSpace(s) == "" {
			bad("title is empty")
		}
	}
	return issues
}

func blockTypes() []string {
	out := make([]string, 0, len(blockFields))
	for t := range blockFields {
		out = append(out, t)
	}
	sort.Strings(out)
	return out
}

func contains(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}
