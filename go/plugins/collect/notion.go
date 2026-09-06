package collect

// Notion over its MCP server: the pages I touched recently. Each is a
// notion_page with its url; its title links it to the tickets and PRs
// it names. Notion is a source people ask bough to read, rarely a link
// they paste, so this stays shallow: enough for "the page about X".

import (
	"encoding/json"
	"strings"

	"github.com/andreylukin/bough/plugins/graph"
)

// Notion collects from the named MCP server.
func (r *Run) Notion(server string) Report {
	rep := Report{Source: "notion"}
	out, err := r.call(server, "notion-list-recent-pages", "{}")
	if err != nil {
		rep.Err = err
		return rep
	}
	for _, p := range notionPages(out) {
		e, err := r.St.Upsert("notion_page", p.id, p.title, "")
		if err != nil {
			rep.Err = err
			return rep
		}
		rep.Entities++
		if _, err := r.St.SetLink(e, graph.Link{URL: p.url, UpdatedAt: when(p.edited)}); err != nil {
			rep.Err = err
			return rep
		}
		n, err := r.linkText(e, p.title, when(p.edited))
		rep.Edges += n
		if err != nil {
			rep.Err = err
			return rep
		}
	}
	return rep
}

type notionPage struct{ id, title, url, edited string }

func notionPages(text string) []notionPage {
	raw := jsonIn(text)
	if raw == "" {
		return nil
	}
	var v any
	if err := json.Unmarshal([]byte(raw), &v); err != nil {
		return nil
	}
	var list []any
	switch t := v.(type) {
	case []any:
		list = t
	case map[string]any:
		for _, k := range []string{"pages", "results", "items", "data"} {
			if l, ok := t[k].([]any); ok {
				list = l
				break
			}
		}
	}
	var out []notionPage
	for _, it := range list {
		m, ok := it.(map[string]any)
		if !ok {
			continue
		}
		p := notionPage{id: str(m, "id", "page_id"), title: str(m, "title", "name"), url: str(m, "url", "public_url"), edited: str(m, "last_edited_time", "lastEditedTime", "updated_at")}
		if p.id == "" && p.url != "" {
			p.id = p.url[strings.LastIndex(p.url, "-")+1:]
		}
		if p.id == "" {
			continue
		}
		out = append(out, p)
	}
	return out
}
