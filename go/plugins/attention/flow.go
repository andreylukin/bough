package attention

// The flow board: the board's items as subject rows over time. Each
// row is one item's last N days as stage-coloured segments with the
// graph's edges as marks, then the item as it is now, then what will
// touch it next. Groups are whose turn it is, so the "now" column read
// top to bottom is the kanban and a row read left to right is the
// timeline.

import (
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/graph"
)

// Stages, in the order a piece of work moves through them.
const (
	StageQueued   = "queued"    // a ticket or page with no PR yet
	StageBuilding = "building"  // a PR being written, or a ticket in progress
	StageReview   = "in review" // review requested, threads open, awaiting someone
	StageBlocked  = "blocked"   // CI failing with nobody on it, or a pr-watch blocker
	StageShipping = "shipping"  // approved or merged within the last day
)

// Segment is one stretch of a row's track in one stage. Agent is set
// while a session or pr-watch job held the item during it.
type Segment struct {
	From  time.Time `json:"from"`
	To    time.Time `json:"to"`
	Stage string    `json:"stage"`
	Agent bool      `json:"agent,omitempty"`
}

// Mark is one graph edge on the track.
type Mark struct {
	At    time.Time `json:"at"`
	Kind  string    `json:"kind"` // me (ball to you), agent, bot, other
	Text  string    `json:"text"`
	Claim string    `json:"claim,omitempty"`
	URL   string    `json:"url,omitempty"` // a session mark opens its chat
}

// Next is something scheduled to touch the row without the person.
type Next struct {
	At   time.Time `json:"at"`
	Text string    `json:"text"`
}

// Row is one item on the flow board.
type Row struct {
	Subject  Subject   `json:"subject"`
	Item     Item      `json:"item"`
	Segments []Segment `json:"segments"`
	Marks    []Mark    `json:"marks"`
	Next     []Next    `json:"next,omitempty"`
}

// Subject is the ticket a PR implements, or the item itself.
type Subject struct {
	Key   string `json:"key"`
	Title string `json:"title"`
	URL   string `json:"url,omitempty"`
}

// Group is one band of rows by whose turn it is.
type Group struct {
	Key   string `json:"key"` // me, motion, blocked, others
	Label string `json:"label"`
	Rows  []Row  `json:"rows"`
}

// Flow is the whole board.
type Flow struct {
	Groups    []Group   `json:"groups"`
	From      time.Time `json:"from"`
	Now       time.Time `json:"now"`
	Collected time.Time `json:"collected"`
	Err       string    `json:"err,omitempty"`
}

// Flow builds the board over the last `days`.
func (s *Service) Flow(days int) Flow {
	if days <= 0 {
		days = 7
	}
	now := s.Now()
	from := now.Add(-time.Duration(days) * 24 * time.Hour)
	b := s.Board()
	f := Flow{From: from, Now: now, Collected: b.Collected, Err: b.Err}
	if b.Empty() {
		return f
	}
	groups := map[string]*Group{
		"me":      {Key: "me", Label: "needs me"},
		"motion":  {Key: "motion", Label: "in motion"},
		"blocked": {Key: "blocked", Label: "blocked"},
		"others":  {Key: "others", Label: "waiting on others"},
	}
	place := func(col string, items []Item) {
		for _, it := range items {
			g := col
			if col == "others" && stageNow(it, col) == StageBlocked {
				g = "blocked"
			}
			row := s.row(it, g, from, now, b.Collected)
			groups[g].Rows = append(groups[g].Rows, row)
		}
	}
	place("me", b.Me)
	place("motion", b.Motion)
	place("others", b.Others)
	for _, k := range []string{"me", "motion", "blocked", "others"} {
		g := groups[k]
		if len(g.Rows) == 0 {
			continue
		}
		// Oldest debt first within a group; rows of one subject stay together.
		sort.SliceStable(g.Rows, func(i, j int) bool {
			if g.Rows[i].Subject.Key != g.Rows[j].Subject.Key {
				return g.Rows[i].Item.Since.Before(g.Rows[j].Item.Since)
			}
			return g.Rows[i].Item.Key < g.Rows[j].Item.Key
		})
		f.Groups = append(f.Groups, *g)
	}
	return f
}

// stageNow is the item's current stage from its facts.
func stageNow(it Item, col string) string {
	switch {
	case it.Session != "":
		return StageBuilding
	case strings.HasPrefix(it.Status, "ci failing") && col != "me" && it.Count == 0:
		return StageBlocked
	case it.Kind == "ticket" && (it.Status == "todo" || it.Status == "backlog" || it.Status == "triage" || it.Status == ""):
		return StageQueued
	case it.Kind == "ticket" && (it.Status == "in progress" || it.Status == "in_progress" || it.Status == "started"):
		return StageBuilding
	case it.Status == "approved" || it.Status == "merged":
		return StageShipping
	default:
		return StageReview
	}
}

// row builds one item's track from its timeline.
func (s *Service) row(it Item, col string, from, now, collected time.Time) Row {
	r := Row{Item: it, Subject: Subject{Key: it.Key, Title: it.Title, URL: it.URL}}
	if it.Count > 0 {
		// A stack: one segment in its stage, no history worth drawing.
		r.Segments = []Segment{{From: maxTime(from, it.Since), To: now, Stage: StageReview}}
		return r
	}
	e, err := s.graph.Store.Get(it.Kind, it.Key)
	if err != nil {
		r.Segments = []Segment{{From: maxTime(from, it.Since), To: now, Stage: stageNow(it, "")}}
		return r
	}
	edges, err := s.graph.Store.Timeline(e)
	if err != nil {
		edges = nil
	}
	// Oldest first for the sweep.
	slices.SortFunc(edges, func(a, b graph.Edge) int { return int(a.ValidFrom - b.ValidFrom) })
	me := s.graph.Me
	who := func(p graph.Entity) string {
		if p.Key == me {
			return "you"
		}
		return strings.TrimSuffix(strings.TrimPrefix(first(p.Title, p.Key), "github:"), "[bot]")
	}
	// Subject: the ticket this PR implements, if any.
	for _, ed := range edges {
		if ed.Rel == "implements" && ed.Src.ID == e.ID && ed.ValidTo == nil {
			r.Subject = Subject{Key: ed.Dst.Key, Title: first(ed.Dst.Title, ed.Dst.Key), URL: ed.Dst.URL}
			break
		}
	}
	// Change points: every edge start or end inside the window changes
	// the stage; the stage at a point is derived from the edges open then.
	type point struct {
		at   int64
		open bool
		e    graph.Edge
	}
	var pts []point
	start := from.Unix()
	for _, ed := range edges {
		switch ed.Rel {
		case "has_state", "awaits", "touches", "reviews", "authored", "assigned":
		default:
			continue
		}
		pts = append(pts, point{ed.ValidFrom, true, ed})
		if ed.ValidTo != nil {
			pts = append(pts, point{*ed.ValidTo, false, ed})
		}
	}
	sort.SliceStable(pts, func(i, j int) bool { return pts[i].at < pts[j].at })
	// Marks: edges that began inside the window.
	for _, ed := range edges {
		if ed.ValidFrom < start || ed.ValidFrom > now.Unix() {
			continue
		}
		m := Mark{At: time.Unix(ed.ValidFrom, 0), Kind: "other", Claim: ed.Claim}
		switch ed.Rel {
		case "authored":
			m.Text = who(ed.Src) + " opened"
		case "assigned":
			m.Text = "assigned " + who(ed.Src)
		case "reviews":
			m.Text = who(ed.Src) + " " + first(ed.Claim, "reviewed")
			if isBotName(ed.Src.Key) {
				m.Kind = "bot"
			}
		case "awaits":
			if ed.Src.ID != e.ID {
				continue
			}
			m.Text = "→ " + who(ed.Dst)
			if ed.Dst.Key == me {
				m.Kind = "me"
			}
		case "touches":
			if ed.Src.Kind != "session" {
				continue
			}
			m.Kind, m.Text, m.URL = "agent", "session", sessionURL(ed.Src.Key)
			if t := strings.TrimPrefix(ed.Src.Title, "exec: "); t != "" {
				if rn := []rune(t); len(rn) > 28 {
					t = string(rn[:28]) + "…"
				}
				m.Text = "session “" + t + "”"
			}
		case "has_state":
			if ed.Dst.Key == "open" {
				continue // "opened" already says it
			}
			m.Text = ed.Dst.Key
		default:
			continue
		}
		r.Marks = append(r.Marks, m)
	}
	// Sweep the points and emit segments.
	open := map[int64]graph.Edge{}
	// A PR is being built until someone is asked to look at it; from
	// then on an open PR is in review even between awaits windows.
	everReviewed := false
	stageAt := func() string {
		var state, awaitsWho string
		for _, ed := range open {
			switch ed.Rel {
			case "has_state":
				state = ed.Dst.Key
			case "awaits":
				if ed.Src.ID == e.ID {
					awaitsWho = ed.Dst.Key
				}
			}
		}
		switch {
		case state == "merged" || state == "approved" || state == "done" || state == "released":
			return StageShipping
		case awaitsWho != "":
			return StageReview
		case it.Kind == "pr" && state == "open" && everReviewed:
			return StageReview
		case state == "draft":
			return StageBuilding
		case state == "" && it.Kind == "pr":
			return "" // not born yet: nothing to draw
		case it.Kind == "ticket":
			switch state {
			case "in progress", "in_progress", "started":
				return StageBuilding
			case "code_review", "in review", "review":
				return StageReview
			case "", "todo", "backlog", "triage":
				return StageQueued
			}
			return StageQueued
		case state == "open":
			return StageBuilding
		}
		return StageQueued
	}
	cur := start
	// Edges open before the window start.
	i := 0
	for ; i < len(pts) && pts[i].at <= start; i++ {
		if pts[i].open {
			open[pts[i].e.ID] = pts[i].e
		} else {
			delete(open, pts[i].e.ID)
		}
	}
	emit := func(to int64) {
		if to <= cur {
			return
		}
		st := stageAt()
		if st == "" {
			cur = to
			return
		}
		if n := len(r.Segments); n > 0 && r.Segments[n-1].Stage == st {
			r.Segments[n-1].To = time.Unix(to, 0)
		} else {
			r.Segments = append(r.Segments, Segment{From: time.Unix(cur, 0), To: time.Unix(to, 0), Stage: st})
		}
		cur = to
	}
	for ; i < len(pts); i++ {
		if pts[i].at > now.Unix() {
			break
		}
		emit(pts[i].at)
		if pts[i].open {
			open[pts[i].e.ID] = pts[i].e
			if pts[i].e.Rel == "awaits" || pts[i].e.Rel == "reviews" {
				everReviewed = true
			}
		} else {
			delete(open, pts[i].e.ID)
		}
	}
	emit(now.Unix())
	// The board's column is the present truth for the tail: a PR the
	// board says waits on others is in review whatever the sweep saw.
	if n := len(r.Segments); n > 0 && (col == "others" || col == "me") && r.Segments[n-1].Stage == StageBuilding && it.Status != "draft" {
		r.Segments[n-1].Stage = StageReview
	}
	if len(r.Segments) == 0 {
		r.Segments = []Segment{{From: from, To: now, Stage: stageNow(it, "")}}
	}
	// The present overrides the sweep's last word: CI and pr-watch are
	// facts the timeline does not carry.
	last := &r.Segments[len(r.Segments)-1]
	if st := stageNow(it, ""); st == StageBlocked || st == StageBuilding && it.Session != "" {
		if st == StageBlocked && last.Stage != StageBlocked {
			// Blocked since the item last changed, at the earliest inside the window.
			cut := maxTime(from, it.Since)
			if cut.After(last.From) && cut.Before(last.To) {
				tail := Segment{From: cut, To: last.To, Stage: StageBlocked}
				last.To = cut
				r.Segments = append(r.Segments, tail)
			} else {
				last.Stage = StageBlocked
			}
		}
	}
	if it.Session != "" {
		// The agent overlay: from when the lock was taken to now.
		cut := maxTime(from, it.Since)
		last = &r.Segments[len(r.Segments)-1]
		if cut.After(last.From) && cut.Before(last.To) {
			tail := Segment{From: cut, To: last.To, Stage: last.Stage, Agent: true}
			last.To = cut
			r.Segments = append(r.Segments, tail)
		} else {
			last.Agent = true
		}
	}
	// pr-watch history as agent marks.
	if s.recent != nil {
		if rc, ok := s.recent(it.Key); ok && rc.At.After(from) {
			r.Marks = append(r.Marks, Mark{At: rc.At, Kind: "agent", Text: "pr-watch", Claim: rc.Summary})
		}
	}
	slices.SortFunc(r.Marks, func(a, b Mark) int { return a.At.Compare(b.At) })
	// Next: what will touch it.
	if it.Session != "" || it.Kind == "pr" {
		if s.nextPoll != nil {
			if t := s.nextPoll(); !t.IsZero() && t.After(now) {
				r.Next = append(r.Next, Next{At: t, Text: "pr-watch poll"})
			}
		}
	}
	if s.collectEvery > 0 && !collected.IsZero() {
		if t := collected.Add(s.collectEvery); t.After(now) {
			r.Next = append(r.Next, Next{At: t, Text: "collect"})
		}
	}
	if strings.HasPrefix(it.Detail, "awaits ") && now.Sub(it.Since) > 14*24*time.Hour {
		r.Next = append(r.Next, Next{At: now, Text: "nudge?"})
	}
	return r
}

func isBotName(key string) bool {
	k := strings.ToLower(key)
	return strings.Contains(k, "[bot]") || strings.Contains(k, "devin") || strings.Contains(k, "codex") || strings.Contains(k, "copilot") || strings.Contains(k, "dependabot")
}

func maxTime(a, b time.Time) time.Time {
	if a.After(b) {
		return a
	}
	return b
}
