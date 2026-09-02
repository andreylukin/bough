package kernel

import (
	"fmt"
	"os"
	"strings"

	"gopkg.in/yaml.v3"
)

// Row is one plugin row in bough.yml. Row order carries no semantics;
// mount order comes from Inject() vs keys already provided.
type Row struct {
	ID       string         `yaml:"id"`
	Plugin   string         `yaml:"plugin"`
	Config   map[string]any `yaml:"config"`
	Disabled bool           `yaml:"disabled"`
}

// LoadFile parses bough.yml: a top-level list of rows.
func LoadFile(path string) ([]Row, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("kernel: read config: %w", err)
	}
	var rows []Row
	if err := yaml.Unmarshal(data, &rows); err != nil {
		return nil, fmt.Errorf("kernel: parse %s: %w", path, err)
	}
	for i, r := range rows {
		if r.ID == "" || r.Plugin == "" {
			return nil, fmt.Errorf("kernel: %s row %d: id and plugin are required", path, i)
		}
	}
	return rows, nil
}

// Mount instantiates and applies every enabled row, ordering by
// dependencies: a row mounts once every key in its plugin's Inject()
// is provided. Fails loud naming the row and missing keys if stuck.
func (c *Context) Mount(rows []Row) error {
	type pending struct {
		row Row
		p   Plugin
	}
	var todo []pending
	for _, r := range rows {
		if r.Disabled {
			continue
		}
		factory, ok := lookup(r.Plugin)
		if !ok {
			return fmt.Errorf("kernel: row %q: unknown plugin %q", r.ID, r.Plugin)
		}
		todo = append(todo, pending{row: r, p: factory()})
	}
	for len(todo) > 0 {
		progressed := false
		var rest []pending
		for _, pd := range todo {
			if missing(c, pd.p) != nil {
				rest = append(rest, pd)
				continue
			}
			if err := pd.p.Apply(c, pd.row.Config); err != nil {
				return fmt.Errorf("kernel: row %q (%s): %w", pd.row.ID, pd.row.Plugin, err)
			}
			progressed = true
		}
		todo = rest
		if !progressed {
			var lines []string
			for _, pd := range todo {
				lines = append(lines, fmt.Sprintf("row %q (%s) missing %v",
					pd.row.ID, pd.row.Plugin, missing(c, pd.p)))
			}
			return fmt.Errorf("kernel: unresolvable dependencies:\n  %s",
				strings.Join(lines, "\n  "))
		}
	}
	return nil
}

func missing(c *Context, p Plugin) []string {
	var m []string
	for _, key := range p.Inject() {
		if !c.has(key) {
			m = append(m, key)
		}
	}
	return m
}
