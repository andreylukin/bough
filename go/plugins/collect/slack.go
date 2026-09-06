package collect

// Slack over its MCP server, selectively: threads that mention me and
// threads that link my open PRs and tickets. Each becomes a
// slack_thread with its permalink; the thread's text links it to what
// it discusses, and it awaits me until my reply is the last word.
//
// The server answers in markdown, not JSON: a profile is "Key: value"
// lines, a search is "### Result n of m" blocks with the thread's
// replies inline under "Context after:". Parsed as such.

import (
	"fmt"
	"regexp"
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
		for _, th := range slackThreads(out) {
			if th.key() == "" || seen[th.key()] {
				continue
			}
			seen[th.key()] = true
			ents, edges, err := r.recordThread(me, th)
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

// slackMe reads my profile (the default user) and aliases me.
func (r *Run) slackMe(server string) slackUser {
	out, err := r.call(server, "slack_read_user_profile", "{}")
	if err != nil {
		return slackUser{}
	}
	f := kvLines(unwrap(out))
	u := slackUser{id: f["user id"], email: strings.ToLower(f["email"]), name: f["real name"]}
	if u.id != "" {
		_ = r.St.Alias(r.Me.ID, "slack", u.id, "")
	}
	return u
}

// unwrap takes the text out of a {"result": "…"} / {"results": "…"}
// envelope, or returns the text as is.
func unwrap(text string) string {
	raw := jsonIn(text)
	if raw == "" {
		return text
	}
	var m map[string]any
	if err := jsonDecode(raw, &m); err != nil {
		return text
	}
	for _, k := range []string{"result", "results", "text", "content"} {
		if s, ok := m[k].(string); ok {
			return s
		}
	}
	return text
}

// kvLines reads "Key: value" lines into a lower-cased map.
func kvLines(text string) map[string]string {
	out := map[string]string{}
	for _, line := range strings.Split(text, "\n") {
		k, v, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		out[strings.ToLower(strings.TrimSpace(k))] = strings.TrimSpace(v)
	}
	return out
}

// slackMsg is one message: who, when, what.
type slackMsg struct {
	userID, email, name, ts, text string
}

// slackThread is one search hit with its replies.
type slackThread struct {
	channel, channelName, threadTS, permalink string
	root                                      slackMsg
	replies                                   []slackMsg
}

func (t slackThread) key() string {
	if t.channel == "" || t.threadTS == "" {
		return ""
	}
	return t.channel + ":" + t.threadTS
}

// last is the message that had the last word.
func (t slackThread) last() slackMsg {
	last := t.root
	for _, m := range t.replies {
		if when(m.ts) >= when(last.ts) {
			last = m
		}
	}
	return last
}

func (t slackThread) allText() string {
	parts := []string{t.root.text}
	for _, m := range t.replies {
		parts = append(parts, m.text)
	}
	return strings.Join(parts, "\n")
}

var (
	resultRe  = regexp.MustCompile(`(?m)^### Result \d+ of \d+\s*$`)
	channelRe = regexp.MustCompile(`Channel: #?([^\s(]+) \(ID: ([A-Z0-9_]+)\)`)
	fromRe    = regexp.MustCompile(`From: ([^<\n]*?)\s*(?:<([^>]*)>)?\s*\(ID: ([A-Z0-9_]+)\)`)
	linkRe    = regexp.MustCompile(`Permalink: \[link\]\(([^)]+)\)`)
	threadRe  = regexp.MustCompile(`[?&]thread_ts=([0-9.]+)`)
	replyRe   = regexp.MustCompile(`(?m)^- From: ([^<\n]*?)\s*(?:<([^>]*)>)?\s*\(ID: ([A-Z0-9_]+)\)\s*\n\s+Message_ts: ([0-9.]+)\s*\n((?:[ \t]+.*\n?)*)`)
)

// slackThreads reads a search reply's result blocks.
func slackThreads(text string) []slackThread {
	body := unwrap(text)
	locs := resultRe.FindAllStringIndex(body, -1)
	var out []slackThread
	for i, loc := range locs {
		end := len(body)
		if i+1 < len(locs) {
			end = locs[i+1][0]
		}
		block := body[loc[1]:end]
		head, ctx, _ := strings.Cut(block, "Context after:")
		var t slackThread
		if m := channelRe.FindStringSubmatch(head); m != nil {
			t.channelName, t.channel = m[1], m[2]
		}
		if m := fromRe.FindStringSubmatch(head); m != nil {
			t.root.name, t.root.email, t.root.userID = strings.TrimSpace(m[1]), strings.ToLower(m[2]), m[3]
		}
		f := kvLines(head)
		t.root.ts = f["message_ts"]
		if m := linkRe.FindStringSubmatch(head); m != nil {
			t.permalink = m[1]
			if tm := threadRe.FindStringSubmatch(m[1]); tm != nil {
				t.threadTS = tm[1]
			}
		}
		if t.threadTS == "" {
			t.threadTS = t.root.ts
		}
		if _, after, ok := strings.Cut(head, "Text:"); ok {
			t.root.text = strings.TrimSpace(after)
		}
		for _, m := range replyRe.FindAllStringSubmatch(ctx, -1) {
			t.replies = append(t.replies, slackMsg{
				name: strings.TrimSpace(m[1]), email: strings.ToLower(m[2]), userID: m[3], ts: m[4],
				text: strings.TrimSpace(unindent(m[5])),
			})
		}
		out = append(out, t)
	}
	return out
}

var mentionRe = regexp.MustCompile(`<@[A-Z0-9_]+\|([^>]+)>|<@[A-Z0-9_]+>`)

// plainMentions turns "<@U0B2L|Andrey Lukin>" into "@Andrey Lukin" and a
// bare "<@U0B2L>" into "@someone", for titles.
func plainMentions(s string) string {
	return mentionRe.ReplaceAllStringFunc(s, func(m string) string {
		if i := strings.Index(m, "|"); i >= 0 {
			return "@" + m[i+1:len(m)-1]
		}
		return "@someone"
	})
}

func unindent(s string) string {
	var lines []string
	for _, l := range strings.Split(s, "\n") {
		lines = append(lines, strings.TrimSpace(l))
	}
	return strings.Join(lines, "\n")
}

func (r *Run) recordThread(me slackUser, t slackThread) (ents, edges int, err error) {
	at := when(t.root.ts)
	title := graph.FirstSentence(plainMentions(t.root.text), 80)
	if t.channelName != "" {
		title = "#" + t.channelName + ": " + title
	}
	e, err := r.St.Upsert("slack_thread", t.key(), title, "")
	if err != nil {
		return 0, 0, err
	}
	ents++
	last := t.last()
	text := t.allText()
	mentionsMe := strings.Contains(text, "<@"+me.id)
	st := "open"
	if last.userID == me.id {
		st = "answered"
	}
	if _, err := r.St.SetLink(e, graph.Link{URL: t.permalink, Status: st, UpdatedAt: when(last.ts)}); err != nil {
		return ents, edges, err
	}
	count := func(ok bool, err error) error {
		if ok {
			edges++
		}
		return err
	}
	if t.root.userID != "" {
		p, err := r.person("slack", t.root.userID, t.root.email, t.root.name)
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
		if err := count(r.assert(e, "awaits", r.Me, when(last.ts), "last message is not mine")); err != nil {
			return ents, edges, err
		}
	} else {
		for _, o := range open {
			if o.Src.ID == e.ID {
				if err := r.St.Invalidate(o.ID, "answered", "collector", when(last.ts)); err != nil {
					return ents, edges, err
				}
			}
		}
	}
	return ents, edges, nil
}
