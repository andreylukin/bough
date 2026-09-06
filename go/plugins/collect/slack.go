package collect

// Slack over its MCP server, selectively: threads that mention me and
// threads that link my open PRs and tickets. Each becomes a
// slack_thread with its permalink; the thread's text links it to what
// it discusses, and it awaits me until my reply is the last word.

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/graph"
)

const slackSearchLimit = 20

// Slack collects from the named MCP server with the default queries
// (mentions of me, my open PR links) plus extra.
func (r *Run) Slack(server string, extra []string) Report {
	rep := Report{Source: "slack"}
	me := r.slackMe(server)
	if me.id == "" {
		rep.Err = fmt.Errorf("slack_read_user_profile: could not read my user id")
		return rep
	}
	after := time.Unix(r.Now, 0).AddDate(0, 0, -r.Days).Format("2006-01-02")
	queries := []string{fmt.Sprintf("<@%s> after:%s", me.id, after)}
	if w, err := r.St.WorldOf(r.Me); err == nil {
		n := 0
		for _, e := range w.Mine {
			if e.Kind == "pr" && e.URL != "" && n < 10 {
				queries = append(queries, e.URL)
				n++
			}
		}
	}
	queries = append(queries, extra...)

	seen := map[string]bool{}
	for _, q := range queries {
		out, err := r.call(server, "slack_search_public_and_private", jsonArgs(map[string]any{"query": q, "limit": slackSearchLimit}))
		if err != nil {
			rep.Err = err
			return rep
		}
		for _, m := range slackMessages(out) {
			key := m.threadKey()
			if key == "" || seen[key] {
				continue
			}
			seen[key] = true
			ents, edges, err := r.recordThread(server, me, m)
			if err != nil {
				rep.Err = err
				return rep
			}
			rep.Entities += ents
			rep.Edges += edges
		}
	}
	return rep
}

type slackUser struct{ id, email, name string }

// slackMe reads my profile (default user) and aliases me.
func (r *Run) slackMe(server string) slackUser {
	out, err := r.call(server, "slack_read_user_profile", "{}")
	if err != nil {
		return slackUser{}
	}
	var m map[string]any
	if raw := jsonIn(out); raw != "" {
		_ = json.Unmarshal([]byte(raw), &m)
	}
	if p, ok := m["profile"].(map[string]any); ok {
		for k, v := range p {
			if _, dup := m[k]; !dup {
				m[k] = v
			}
		}
	}
	u := slackUser{id: str(m, "id", "user_id"), email: str(m, "email"), name: str(m, "real_name", "display_name", "name")}
	if u.id != "" {
		_ = r.St.Alias(r.Me.ID, "slack", u.id, "")
	}
	return u
}

// slackMessage is one search hit or thread reply.
type slackMessage struct {
	Channel, ChannelName, TS, ThreadTS, User, Text, Permalink string
}

func (m slackMessage) threadKey() string {
	ts := m.ThreadTS
	if ts == "" {
		ts = m.TS
	}
	if m.Channel == "" || ts == "" {
		return ""
	}
	return m.Channel + ":" + ts
}

// slackMessages reads the messages in a search or thread reply.
func slackMessages(text string) []slackMessage {
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
		for _, k := range []string{"messages", "matches", "results", "items"} {
			if l, ok := t[k].([]any); ok {
				list = l
				break
			}
			if mm, ok := t[k].(map[string]any); ok {
				if l, ok := mm["matches"].([]any); ok {
					list = l
					break
				}
			}
		}
	}
	var out []slackMessage
	for _, it := range list {
		m, ok := it.(map[string]any)
		if !ok {
			continue
		}
		sm := slackMessage{
			TS:        str(m, "ts", "timestamp"),
			ThreadTS:  str(m, "thread_ts"),
			User:      str(m, "user", "user_id"),
			Text:      str(m, "text"),
			Permalink: str(m, "permalink", "url"),
		}
		switch c := m["channel"].(type) {
		case string:
			sm.Channel = c
		case map[string]any:
			sm.Channel = str(c, "id")
			sm.ChannelName = str(c, "name")
		}
		if sm.Channel == "" {
			sm.Channel = str(m, "channel_id")
		}
		if sm.TS == "" && sm.Permalink != "" {
			// …/archives/C0123/p1725600000000100
			if i := strings.LastIndex(sm.Permalink, "/p"); i >= 0 && len(sm.Permalink) > i+12 {
				d := sm.Permalink[i+2:]
				if len(d) > 6 {
					sm.TS = d[:len(d)-6] + "." + d[len(d)-6:]
				}
			}
		}
		out = append(out, sm)
	}
	return out
}

func (r *Run) recordThread(server string, me slackUser, m slackMessage) (ents, edges int, err error) {
	key := m.threadKey()
	at := when(m.TS)
	title := graph.FirstSentence(m.Text, 80)
	if m.ChannelName != "" {
		title = "#" + m.ChannelName + ": " + title
	}
	e, err := r.St.Upsert("slack_thread", key, title, "")
	if err != nil {
		return 0, 0, err
	}
	ents++
	// The whole thread: who spoke last decides whether it awaits me.
	last := m
	text := m.Text
	args := jsonArgs(map[string]any{"channel_id": m.Channel, "thread_ts": strings.TrimPrefix(key, m.Channel+":")})
	if out, err := r.call(server, "slack_read_thread", args); err == nil {
		msgs := slackMessages(out)
		for _, x := range msgs {
			text += "\n" + x.Text
			if when(x.TS) >= when(last.TS) {
				last = x
			}
		}
	}
	mentionsMe := strings.Contains(text, "<@"+me.id+">")
	st := "open"
	if last.User == me.id {
		st = "answered"
	}
	if _, err := r.St.SetLink(e, graph.Link{URL: m.Permalink, Status: st, UpdatedAt: when(last.TS)}); err != nil {
		return ents, edges, err
	}
	count := func(ok bool, err error) error {
		if ok {
			edges++
		}
		return err
	}
	if m.User != "" {
		p, err := r.person("slack", m.User, "", "")
		if err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(p, "authored", e, at, "")); err != nil {
			return ents, edges, err
		}
	}
	if mentionsMe {
		if err := count(r.assert(e, "mentions", r.Me, at, "")); err != nil {
			return ents, edges, err
		}
	}
	n, err := r.linkText(e, text, at)
	edges += n
	if err != nil {
		return ents, edges, err
	}
	// It awaits me while someone else had the last word and it either
	// names me or is about something of mine.
	open, err := r.St.Neighbors(e, 1, "awaits", 0)
	if err != nil {
		return ents, edges, err
	}
	if st == "open" && (mentionsMe || n > 0) {
		if err := count(r.assert(e, "awaits", r.Me, when(last.TS), "last message is not mine")); err != nil {
			return ents, edges, err
		}
	} else {
		for _, o := range open {
			if o.Src.ID == e.ID {
				if err := r.St.Invalidate(o.ID, "answered", "collector", when(last.TS)); err != nil {
					return ents, edges, err
				}
			}
		}
	}
	return ents, edges, nil
}
