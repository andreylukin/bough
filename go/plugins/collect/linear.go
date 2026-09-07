package collect

// Linear over its MCP server: the issues assigned to me that moved
// recently. Fills the ticket titles the backfill left empty, records
// the state history, and links the PRs an issue's text names.

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/andreylukin/bough/plugins/graph"
)

// Linear collects from the named MCP server.
func (r *Run) Linear(server string) Report {
	rep := Report{Source: "linear"}
	args := fmt.Sprintf(`{"assignee":"me","updatedAt":"-P%dD","limit":100}`, r.Days)
	out, err := r.call(server, "list_issues", args)
	if err != nil {
		rep.Err = err
		return rep
	}
	issues := linearIssues(out)
	if len(issues) == 0 && strings.TrimSpace(out) != "" && jsonIn(out) == "" {
		rep.Err = fmt.Errorf("list_issues: no JSON in the reply: %s", firstLine(out))
		return rep
	}
	for _, is := range issues {
		ents, edges, err := r.recordIssue(is)
		if err != nil {
			rep.Err = err
			return rep
		}
		rep.Entities += ents
		rep.Edges += edges
	}
	return rep
}

// linearIssue is the part of an issue the graph keeps.
type linearIssue struct {
	ID, Identifier, Title, URL, State, Description, Branch, UpdatedAt string
	CreatedAt, StartedAt, CompletedAt                                 string
	Assignee                                                          struct{ ID, Name, Email string }
	Links                                                             []string // attachment / PR urls
}

// linearIssues reads list_issues' reply: a JSON array, or an object
// with an "issues"/"nodes"/"data" array, with the field names Linear
// uses. Unknown shapes yield nothing rather than junk.
func linearIssues(text string) []linearIssue {
	raw := jsonIn(text)
	if raw == "" {
		return nil
	}
	var any_ any
	dec := json.NewDecoder(strings.NewReader(raw))
	if err := dec.Decode(&any_); err != nil {
		return nil
	}
	var list []any
	switch v := any_.(type) {
	case []any:
		list = v
	case map[string]any:
		for _, k := range []string{"issues", "nodes", "data", "results", "items"} {
			if l, ok := v[k].([]any); ok {
				list = l
				break
			}
		}
		if list == nil {
			if _, ok := v["identifier"]; ok {
				list = []any{v}
			}
		}
	}
	var out []linearIssue
	for _, it := range list {
		m, ok := it.(map[string]any)
		if !ok {
			continue
		}
		// The MCP server puts the human identifier in "id" and the
		// uuid in "uuid"; the GraphQL API has "identifier" and "id".
		is := linearIssue{
			ID:          str(m, "uuid"),
			Identifier:  str(m, "identifier", "key"),
			Title:       str(m, "title"),
			URL:         str(m, "url"),
			State:       str(m, "status", "state"),
			Description: str(m, "description"),
			Branch:      str(m, "gitBranchName"),
			UpdatedAt:   str(m, "updatedAt", "updated_at"),
			CreatedAt:   str(m, "createdAt", "created_at"),
			StartedAt:   str(m, "startedAt", "started_at"),
			CompletedAt: str(m, "completedAt", "completed_at"),
		}
		if id := str(m, "id"); is.Identifier == "" && len(graph.Tickets(id)) == 1 {
			is.Identifier = id
		} else if is.ID == "" {
			is.ID = id
		}
		switch a := m["assignee"].(type) {
		case map[string]any:
			is.Assignee.ID = str(a, "id")
			is.Assignee.Name = str(a, "name", "displayName")
			is.Assignee.Email = str(a, "email")
		case string:
			is.Assignee.Name = a
			is.Assignee.ID = str(m, "assigneeId")
		}
		if l, ok := m["attachments"].([]any); ok {
			for _, a := range l {
				if am, ok := a.(map[string]any); ok {
					if u := str(am, "url"); u != "" {
						is.Links = append(is.Links, u)
					}
				}
			}
		}
		if is.Identifier == "" {
			continue
		}
		out = append(out, is)
	}
	return out
}

func (r *Run) recordIssue(is linearIssue) (ents, edges int, err error) {
	at := when(is.UpdatedAt)
	// The state's own clock when Linear gives one: created for the
	// backlog, started for in-progress, completed for done; updatedAt
	// otherwise (a review state, say).
	born := when(is.CreatedAt)
	if born == 0 {
		born = at
	}
	stateAt := at
	switch st := status(is.State); {
	case st == "done" || st == "canceled" || st == "cancelled" || st == "released":
		if t := when(is.CompletedAt); t > 0 {
			stateAt = t
		}
	case st == "in progress" || st == "in_progress" || st == "started":
		if t := when(is.StartedAt); t > 0 {
			stateAt = t
		}
	case st == "todo" || st == "backlog" || st == "triage" || st == "":
		stateAt = born
	}
	e, err := r.St.Upsert("ticket", is.Identifier, is.Title, "")
	if err != nil {
		return 0, 0, err
	}
	ents++
	if is.ID != "" {
		_ = r.St.Alias(e.ID, "linear", is.ID, is.URL)
	}
	summary := graph.FirstSentence(is.Description, 140)
	if _, err := r.St.SetLink(e, graph.Link{URL: is.URL, Summary: summary, UpdatedAt: at}); err != nil {
		return ents, edges, err
	}
	if err := r.St.SetState(e, status(is.State), r.Ep, "collector", stateAt); err != nil {
		return ents, edges, err
	}
	count := func(ok bool, err error) error {
		if ok {
			edges++
		}
		return err
	}
	// list_issues was asked for assignee=me, so the assignee IS me:
	// its id becomes my Linear alias, whatever the reply calls it.
	if is.Assignee.ID != "" {
		_ = r.St.Alias(r.Me.ID, "linear", is.Assignee.ID, "")
	}
	if err := count(r.assert(r.Me, "assigned", e, born, "")); err != nil {
		return ents, edges, err
	}
	for _, p := range graph.PRLinks(is.Description + " " + is.Branch + " " + strings.Join(is.Links, " ")) {
		pe, err := r.pr(p, "", "")
		if err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(pe, "implements", e, at, "linked from the issue")); err != nil {
			return ents, edges, err
		}
	}
	return ents, edges, nil
}

func firstLine(s string) string {
	s, _, _ = strings.Cut(strings.TrimSpace(s), "\n")
	if len(s) > 160 {
		s = s[:160] + "…"
	}
	return s
}
