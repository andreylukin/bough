package graph

// The world: what the external sources say is open around one person,
// grouped by what it asks of them. A query over open edges, not a
// file, so it is as fresh as the last collector run and never edited
// by hand.

import (
	"fmt"
	"slices"
	"strings"
	"time"
)

// World is the current status of the external world around a person.
type World struct {
	Me       Entity
	AwaitsMe []Entity // I must act: review requested, reply owed, ticket to pick up
	Mine     []Entity // open things I authored or am assigned, with who they await
	Awaiting map[int64][]Entity
	Fresh    int64 // newest collector observation, 0 when there is none
}

// closed are the statuses that take a thing out of the world.
var closed = map[string]bool{
	"merged": true, "closed": true, "done": true, "canceled": true, "cancelled": true,
	"duplicate": true, "answered": true, "resolved": true, "released": true,
}

// IsOpen reports whether an entity's status keeps it in the world.
func IsOpen(e Entity) bool { return !closed[strings.ToLower(e.Status)] }

// WorldOf gathers the world around me from open edges: awaits (them →
// me), authored/assigned (me → them), and, for each of mine, who it
// awaits.
func (s *Store) WorldOf(me Entity) (World, error) {
	w := World{Me: me, Awaiting: map[int64][]Entity{}}
	edges, err := s.Neighbors(me, 1, "", 0)
	if err != nil {
		return w, err
	}
	// Two passes: what awaits me first, so a thing of mine that also
	// awaits me lands under "waiting on me", the list that matters.
	seen := map[int64]bool{}
	// Fresh is the last collector run, whether or not it wrote anything.
	_ = s.db.QueryRow(`SELECT COALESCE(MAX(ingested_at), 0) FROM episodes WHERE source = 'collector' OR source LIKE 'collector:%'`).Scan(&w.Fresh)
	for _, e := range edges {
		if e.Rel == "awaits" && e.Dst.ID == me.ID && IsOpen(e.Src) && !seen[e.Src.ID] {
			seen[e.Src.ID] = true
			w.AwaitsMe = append(w.AwaitsMe, e.Src)
		}
	}
	for _, e := range edges {
		if (e.Rel == "authored" || e.Rel == "assigned") && e.Src.ID == me.ID && IsOpen(e.Dst) && !seen[e.Dst.ID] {
			seen[e.Dst.ID] = true
			w.Mine = append(w.Mine, e.Dst)
		}
	}
	for _, m := range slices.Concat(w.AwaitsMe, w.Mine) {
		out, err := s.Neighbors(m, 1, "awaits", 0)
		if err != nil {
			return w, err
		}
		for _, e := range out {
			if e.Src.ID == m.ID && e.Dst.ID != me.ID {
				w.Awaiting[m.ID] = append(w.Awaiting[m.ID], e.Dst)
			}
		}
	}
	byKind := func(a, b Entity) int {
		if c := strings.Compare(a.Kind, b.Kind); c != 0 {
			return c
		}
		return int(b.UpdatedAt - a.UpdatedAt)
	}
	slices.SortFunc(w.AwaitsMe, byKind)
	slices.SortFunc(w.Mine, byKind)
	return w, nil
}

// Empty is a world with nothing open.
func (w World) Empty() bool { return len(w.AwaitsMe) == 0 && len(w.Mine) == 0 }

// Render is the world as prompt/CLI text: one line per thing, key,
// title, status, who it waits on, and the link. The link is the point:
// the model quotes it instead of searching for it.
func (w World) Render() string {
	if w.Empty() {
		return ""
	}
	var b strings.Builder
	line := func(e Entity, tail string) {
		fmt.Fprintf(&b, "- %s", e.Kind+":"+e.Key)
		if e.Title != "" && e.Title != e.Key {
			fmt.Fprintf(&b, " “%s”", truncate(e.Title, 70))
		}
		if e.Status != "" {
			fmt.Fprintf(&b, " [%s]", e.Status)
		}
		if tail != "" {
			b.WriteString(" " + tail)
		}
		if e.Summary != "" {
			b.WriteString(" — " + truncate(e.Summary, 120))
		}
		if e.URL != "" {
			b.WriteString(" " + e.URL)
		}
		b.WriteString("\n")
	}
	if len(w.AwaitsMe) > 0 {
		b.WriteString("Waiting on me:\n")
		for _, e := range w.AwaitsMe {
			line(e, w.awaitsTail(e))
		}
	}
	if len(w.Mine) > 0 {
		b.WriteString("Mine, open:\n")
		for _, e := range w.Mine {
			line(e, w.awaitsTail(e))
		}
	}
	if w.Fresh > 0 {
		fmt.Fprintf(&b, "(collected %s)\n", time.Unix(w.Fresh, 0).Format("2006-01-02 15:04"))
	}
	return strings.TrimRight(b.String(), "\n")
}

// awaitsTail names the others a thing waits on ("awaits Bradley").
func (w World) awaitsTail(e Entity) string {
	var who []string
	for _, p := range w.Awaiting[e.ID] {
		who = append(who, personLabel(p))
	}
	if len(who) == 0 {
		return ""
	}
	return "awaits " + strings.Join(who, ", ")
}

func personLabel(p Entity) string {
	if p.Title != "" && p.Title != p.Key {
		return p.Title
	}
	return p.Key
}
