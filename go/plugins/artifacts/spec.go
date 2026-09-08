package artifacts

// The spec: a page is a title, optional data sets, and a list of
// blocks; each block is a small typed object. The model writes the
// spec, this file checks it and says exactly what is wrong (block
// number, type, field), and the viewer draws it. Everything about how
// a page looks lives in the viewer, so the model spends its tokens on
// content only.

import (
	"errors"
	"fmt"
	"regexp"
	"sort"
	"strings"
)

// Guide is the spec reference the model reads. It is prompt text, kept
// short: one line per block, the shorthand, and the rules a validator
// would otherwise have to explain after the fact.
const Guide = `A page is {title, subtitle?, data?, blocks: [...]}. A block is an object with "type", or a plain string (= markdown text).
data: {name: [{col: value, ...}, ...]} — rows given ONCE; table/chart/stats/filter blocks bind with from: name instead of repeating them.
Content blocks:
- text {md}: markdown — paragraphs, **bold**, _em_, ` + "`code`" + `, [links](url), "- " lists.
- heading {text} · list {items, ordered?} · kv {items: [{key, value}]} · callout {tone: info|good|warn|bad, title?, text} · code {lang?, text} · timeline {items: [{when, what, tone?}]}
- stats {items: [{label, value, delta?, tone?, spark?: [numbers]}]}: big-number tiles, optional sparkline.
- table {columns?, rows | from, align?, caption?}: rows are arrays or objects keyed by column; sortable, searchable, paged.
- chart {kind: bar|line|area|pie|scatter, x, series | y | from, title?, unit?, stacked?}: inline: x: [..], series: [{name, values}] (or y: [..] for one series); bound: from: name, x: "col", series: ["col", ..] (or y: "col").
- filter {from, by: ["col", ..]}: dropdowns that narrow every block bound to that data.
- diff {text, file?}: a unified diff · image {src, alt?, caption?} · diagram {mermaid}: a mermaid diagram.
Layout: grid {columns: 2|3|4, blocks} · section {title, blocks, collapsed?} · tabs {tabs: [{title, blocks}]}.
Ask the user (answers come back to you as a notice, and via tools.artifactAnswers(name)):
- decision {id, question, options: [..], multi?}: pick one (or several); the user can add a reason.
- checklist {id, items: [..]}: tick items off.
- form {id, fields: [{name, label?, type?: text|textarea|number|select|toggle, options?, placeholder?}], submit?}.
Every page also has a note box the user can send you.
Rules: vary block types (a page of look-alike tables reads as a dump); put the finding in stats or a callout before the evidence; never restate the page title in a heading; no process or model metadata in the page. To change a published page use tools.artifactPatch(name, ops) — ops: [{op: set|append|remove, path: "blocks[2].rows" | "data.sales" | "blocks", value?}] — instead of resending it.`

// Block types and their required / optional fields.
var blockFields = map[string]struct{ required, optional []string }{
	"text":      {[]string{"md"}, nil},
	"heading":   {[]string{"text"}, nil},
	"stats":     {[]string{"items"}, nil},
	"table":     {nil, []string{"columns", "rows", "from", "align", "caption"}},
	"chart":     {[]string{"kind"}, []string{"x", "series", "y", "from", "title", "unit", "stacked"}},
	"filter":    {[]string{"from", "by"}, nil},
	"list":      {[]string{"items"}, []string{"ordered"}},
	"kv":        {[]string{"items"}, nil},
	"callout":   {[]string{"text"}, []string{"tone", "title"}},
	"code":      {[]string{"text"}, []string{"lang"}},
	"diff":      {[]string{"text"}, []string{"file"}},
	"image":     {[]string{"src"}, []string{"alt", "caption"}},
	"diagram":   {[]string{"mermaid"}, nil},
	"timeline":  {[]string{"items"}, nil},
	"grid":      {[]string{"blocks"}, []string{"columns"}},
	"section":   {[]string{"title", "blocks"}, []string{"collapsed"}},
	"tabs":      {[]string{"tabs"}, nil},
	"decision":  {[]string{"id", "question", "options"}, []string{"multi"}},
	"checklist": {[]string{"id", "items"}, nil},
	"form":      {[]string{"id", "fields"}, []string{"submit"}},
}

var chartKinds = []string{"bar", "line", "area", "pie", "scatter"}
var tones = []string{"info", "good", "warn", "bad"}
var fieldTypes = []string{"text", "textarea", "number", "select", "toggle"}
var idRe = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

// Normalize checks a page spec and returns it with shorthand expanded
// (a string block becomes a text block, object rows become arrays, a
// y: becomes a series). The error lists every problem found, one per
// line, so a repair takes one round trip.
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
		case "title", "subtitle", "blocks", "data", "published", "version":
		default:
			issues = append(issues, fmt.Sprintf("%s: unknown page field (page fields are title, subtitle, data, blocks)", k))
		}
	}
	n := &normalizer{data: map[string][]string{}, ids: map[string]bool{}}
	if d, present := page["data"]; present {
		if err := n.checkData(d); err != nil {
			issues = append(issues, err.Error())
		}
	}
	blocks, err := n.blocks(page["blocks"], "blocks")
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
	if d, ok := page["data"].(map[string]any); ok && len(d) > 0 {
		out["data"] = d
	}
	return out, nil
}

// normalizer carries what the checks need to see across blocks: the
// data sets and their columns, and the answer ids used so far.
type normalizer struct {
	data map[string][]string // set name -> columns
	ids  map[string]bool
}

// checkData validates the page's data sets: named arrays of objects,
// every row keyed by the same columns as the first.
func (n *normalizer) checkData(v any) error {
	sets, ok := v.(map[string]any)
	if !ok {
		return errors.New("data: must be {name: [rows]}")
	}
	var issues []string
	for name, rows := range sets {
		list, ok := rows.([]any)
		if !ok || len(list) == 0 {
			issues = append(issues, fmt.Sprintf("data.%s: must be a non-empty array of {col: value} rows", name))
			continue
		}
		var cols []string
		for i, r := range list {
			row, ok := r.(map[string]any)
			if !ok {
				issues = append(issues, fmt.Sprintf("data.%s[%d]: must be an object keyed by column", name, i))
				break
			}
			if i == 0 {
				for c := range row {
					cols = append(cols, c)
				}
				sort.Strings(cols)
			}
		}
		n.data[name] = cols
	}
	if len(issues) > 0 {
		return errors.New(strings.Join(issues, "\n  "))
	}
	return nil
}

// blocks checks a block list; path names it in messages ("blocks",
// "blocks[3].blocks").
func (n *normalizer) blocks(v any, path string) ([]any, error) {
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
		if more := n.check(typ, m, at); len(more) > 0 {
			issues = append(issues, more...)
		}
		switch typ {
		case "grid", "section":
			inner, err := n.blocks(m["blocks"], at+".blocks")
			if err != nil {
				issues = append(issues, err.Error())
			} else {
				m["blocks"] = inner
			}
		case "tabs":
			tabs, _ := m["tabs"].([]any)
			for j, t := range tabs {
				tab, ok := t.(map[string]any)
				if !ok {
					continue
				}
				inner, err := n.blocks(tab["blocks"], fmt.Sprintf("%s.tabs[%d].blocks", at, j))
				if err != nil {
					issues = append(issues, err.Error())
				} else {
					tab["blocks"] = inner
				}
			}
		}
		out = append(out, m)
	}
	if len(issues) > 0 {
		return nil, errors.New(strings.Join(issues, "\n  "))
	}
	return out, nil
}

// check is the per-type shape check beyond field presence; it also
// rewrites lenient shapes into the canonical one.
func (n *normalizer) check(typ string, m map[string]any, at string) []string {
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
	objects := func(f string, need ...string) []map[string]any {
		var out []map[string]any
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
			out = append(out, o)
		}
		return out
	}
	// from: a data set this block binds to; its columns, when known.
	from, bound := str("from")
	var cols []string
	if bound {
		c, ok := n.data[from]
		if !ok {
			names := make([]string, 0, len(n.data))
			for k := range n.data {
				names = append(names, k)
			}
			sort.Strings(names)
			bad("from: no data set %q (data has: %s)", from, strings.Join(names, ", "))
		}
		cols = c
	}
	col := func(f, c string) {
		if len(cols) > 0 && !contains(cols, c) {
			bad("%s: %q is not a column of %s (columns: %s)", f, c, from, strings.Join(cols, ", "))
		}
	}
	answerID := func() {
		id, _ := str("id")
		if !idRe.MatchString(id) {
			bad("id must be a short slug (letters, digits, - and _), unique on the page")
		} else if n.ids[id] {
			bad("id %q is used twice on the page", id)
		}
		n.ids[id] = true
	}
	switch typ {
	case "text", "heading", "code", "diff", "diagram":
		f := map[string]string{"text": "md", "heading": "text", "code": "text", "diff": "text", "diagram": "mermaid"}[typ]
		if s, ok := str(f); ok && strings.TrimSpace(s) == "" {
			bad("%s is empty", f)
		} else if !ok && m[f] != nil {
			bad("%s must be a string", f)
		}
	case "image":
		if s, _ := str("src"); !strings.HasPrefix(s, "data:") && !strings.HasPrefix(s, "http") && !strings.HasPrefix(s, "/") {
			bad("src must be a data: URI or a URL")
		}
	case "stats":
		for i, o := range objects("items", "label", "value") {
			if sp, present := o["spark"]; present {
				if l, ok := sp.([]any); !ok || len(l) < 2 {
					bad("items[%d]: spark must be an array of at least 2 numbers", i)
				}
			}
		}
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
	case "filter":
		if !bound {
			bad("from is required")
		}
		for _, b := range items("by") {
			if c, ok := b.(string); ok {
				col("by", c)
			} else {
				bad("by must list column names")
			}
		}
	case "table":
		n.checkTable(m, at, bound, cols, bad, col)
	case "chart":
		n.checkChart(m, at, bound, cols, bad, col)
	case "grid":
		if c, present := m["columns"]; present {
			if v, ok := c.(float64); !ok || v < 2 || v > 4 {
				bad("columns must be 2, 3 or 4")
			}
		}
	case "section":
		if s, ok := str("title"); ok && strings.TrimSpace(s) == "" {
			bad("title is empty")
		}
	case "tabs":
		objects("tabs", "title", "blocks")
	case "decision":
		answerID()
		if len(items("options")) < 2 {
			bad("options needs at least 2 entries")
		}
	case "checklist":
		answerID()
		items("items")
	case "form":
		answerID()
		seen := map[string]bool{}
		for i, f := range objects("fields", "name") {
			name, _ := f["name"].(string)
			if name == "" || seen[name] {
				bad("fields[%d]: name must be present and unique", i)
			}
			seen[name] = true
			if t, ok := f["type"].(string); ok && !contains(fieldTypes, t) {
				bad("fields[%d]: type must be one of %s", i, strings.Join(fieldTypes, "|"))
			}
			if t, _ := f["type"].(string); t == "select" {
				if o, ok := f["options"].([]any); !ok || len(o) == 0 {
					bad("fields[%d]: a select needs options", i)
				}
			}
		}
	}
	return issues
}

// checkTable accepts rows as arrays or as objects keyed by column and
// leaves arrays behind.
func (n *normalizer) checkTable(m map[string]any, at string, bound bool, cols []string, bad func(string, ...any), col func(string, string)) {
	columns, _ := m["columns"].([]any)
	var names []string
	for _, c := range columns {
		if s, ok := c.(string); ok {
			names = append(names, s)
		}
	}
	if bound {
		for _, c := range names {
			col("columns", c)
		}
		if _, present := m["rows"]; present {
			bad("give rows or from, not both")
		}
		return
	}
	rows, ok := m["rows"].([]any)
	if !ok {
		bad("rows (or from) is required")
		return
	}
	if len(rows) == 0 {
		bad("rows is empty")
		return
	}
	// Object rows: derive columns when absent, then flatten.
	if first, isObj := rows[0].(map[string]any); isObj {
		if len(names) == 0 {
			for k := range first {
				names = append(names, k)
			}
			sort.Strings(names)
			cs := make([]any, len(names))
			for i, c := range names {
				cs[i] = c
			}
			m["columns"] = cs
		}
		flat := make([]any, len(rows))
		for i, r := range rows {
			row, ok := r.(map[string]any)
			if !ok {
				bad("rows[%d]: mixed row shapes", i)
				return
			}
			cells := make([]any, len(names))
			for j, c := range names {
				cells[j] = row[c]
			}
			flat[i] = cells
		}
		m["rows"] = flat
		return
	}
	if len(names) == 0 {
		bad("columns is required with array rows")
	}
	for i, r := range rows {
		row, ok := r.([]any)
		if !ok {
			bad("rows[%d] must be an array of cells", i)
			continue
		}
		if len(names) > 0 && len(row) != len(names) {
			bad("rows[%d] has %d cells, %d columns", i, len(row), len(names))
		}
	}
	if a, present := m["align"]; present {
		if al, ok := a.([]any); !ok || (len(names) > 0 && len(al) != len(names)) {
			bad("align must list one of left|right|center per column")
		}
	}
}

// checkChart accepts y: as a one-series shorthand and, when bound,
// column names for x and series.
func (n *normalizer) checkChart(m map[string]any, at string, bound bool, cols []string, bad func(string, ...any), col func(string, string)) {
	kind, _ := m["kind"].(string)
	if !contains(chartKinds, kind) {
		bad("kind must be one of %s", strings.Join(chartKinds, "|"))
	}
	if y, present := m["y"]; present {
		if _, has := m["series"]; has {
			bad("give series or y, not both")
		}
		m["series"] = []any{map[string]any{"values": y}}
		if s, ok := y.(string); ok && bound {
			m["series"] = []any{s}
		}
		delete(m, "y")
	}
	if bound {
		if x, ok := m["x"].(string); ok {
			col("x", x)
		} else if kind != "scatter" {
			bad("x must name a column of %s", m["from"])
		}
		series, ok := m["series"].([]any)
		if !ok || len(series) == 0 {
			bad("series must list value columns of %s", m["from"])
		}
		for i, s := range series {
			c, ok := s.(string)
			if !ok {
				bad("series[%d] must be a column name when from is set", i)
				continue
			}
			col("series", c)
		}
		return
	}
	x, _ := m["x"].([]any)
	if kind != "scatter" && len(x) == 0 {
		bad("x is required for %s charts (the categories or time points)", kind)
	}
	series, ok := m["series"].([]any)
	if !ok || len(series) == 0 {
		bad("series (or y) is required")
		return
	}
	for i, s := range series {
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
		if _, ok := o["name"]; !ok && len(series) > 1 {
			bad("series[%d]: missing name", i)
		}
	}
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
