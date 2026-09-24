package tracecheck

import (
	"os"
	"path/filepath"
	"testing"
)

// Every checked-in spec graph: the walks replay through Check (so the
// browser and server tests only ever walk paths the model allows) and
// together reach everything they claim to cover.
func TestWalksCoverAndReplay(t *testing.T) {
	t.Parallel()
	dirs, _ := filepath.Glob("../testdata/*")
	for _, dir := range dirs {
		if _, err := os.Stat(filepath.Join(dir, "nodes_000000_of_000000.pb")); err != nil {
			continue
		}
		t.Run(filepath.Base(dir), func(t *testing.T) {
			t.Parallel()
			g, err := Load(dir)
			if err != nil {
				t.Fatal(err)
			}
			for _, cover := range []Cover{CoverStates, CoverTransitions} {
				walks := g.Walks(cover, 0)
				if len(walks) == 0 {
					t.Fatalf("%s: no walks", cover)
				}
				gotStates, gotLinks := map[int]bool{0: true}, map[int]bool{}
				for i, w := range walks {
					if v := g.Check(w.Trace); v != nil {
						t.Fatalf("%s walk %d rejected: %v", cover, i, v)
					}
					for _, l := range w.Links {
						gotLinks[l] = true
						gotStates[g.Links[l].Dest] = true
					}
				}
				reach := g.bfs([]int{0}, func(Link) bool { return true })
				for n := range reach {
					if cover == CoverStates && g.Nodes[n].Name == "yield" && !gotStates[n] {
						t.Errorf("%s: settled state %d never reached", cover, n)
					}
				}
				if cover == CoverTransitions {
					missed := 0
					for i, l := range g.Links {
						if _, ok := reach[l.Src]; ok && !gotLinks[i] {
							missed++
						}
					}
					// A link out of an intermediate node that only a
					// different fork choice reaches can be left out; more
					// than a handful means the walker is broken.
					if missed > len(g.Links)/100+1 {
						t.Errorf("%s: %d of %d links never taken", cover, missed, len(g.Links))
					}
				}
			}
		})
	}
}
