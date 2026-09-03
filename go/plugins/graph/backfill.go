package graph

// Backfill from what bough already holds (docs/graph-memory.md,
// Migration step 2): the old ~/.bough/bough.db (notes → concept entities,
// section_citations → cites edges, command_history → command and repo
// entities with touches edges) and the session log directory (one
// session entity per file, touches the repo it ran in). Each source is
// one episode; re-running is idempotent because keys are deterministic
// and Assert de-duplicates open edges.

import (
	"bufio"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// BackfillReport says what a backfill did.
type BackfillReport struct {
	Concepts, Cites, Commands, Repos, Sessions int
	Skipped                                    []string // sources not present, with why
}

// Backfill ingests the old database (boughDB, "" = skip) and the session
// history directory (histDir, "" = skip).
func (s *Store) Backfill(boughDB, histDir string) (BackfillReport, error) {
	var r BackfillReport
	if boughDB != "" {
		if _, err := os.Stat(boughDB); err != nil {
			r.Skipped = append(r.Skipped, boughDB+": "+err.Error())
		} else if err := s.backfillBoughDB(boughDB, &r); err != nil {
			return r, err
		}
	}
	if histDir != "" {
		if _, err := os.Stat(histDir); err != nil {
			r.Skipped = append(r.Skipped, histDir+": "+err.Error())
		} else if err := s.backfillSessions(histDir, &r); err != nil {
			return r, err
		}
	}
	return r, nil
}

func (s *Store) backfillBoughDB(path string, r *BackfillReport) error {
	old, err := sql.Open("sqlite", "file:"+path+"?mode=ro")
	if err != nil {
		return err
	}
	defer old.Close()
	ep, err := s.Episode("backfill:bough.db", path)
	if err != nil {
		return err
	}

	// notes → concept entities. The note path is the slug (`nased`,
	// `gitops:promotion`); the citations hang off note_sections.
	noteOf := map[int64]Entity{}
	rows, err := old.Query(`SELECT id, path, title, created_at FROM notes`)
	if err == nil {
		for rows.Next() {
			var id, created int64
			var p, title string
			if err := rows.Scan(&id, &p, &title, &created); err != nil {
				rows.Close()
				return err
			}
			e, err := s.Upsert("concept", p, title, "")
			if err != nil {
				rows.Close()
				return err
			}
			noteOf[id] = e
			r.Concepts++
		}
		rows.Close()
	}
	rows, err = old.Query(`SELECT c.kind, c.ref, c.at, sec.note_id FROM section_citations c JOIN note_sections sec ON sec.id = c.section_id`)
	if err == nil {
		for rows.Next() {
			var kind, ref string
			var at, noteID int64
			if err := rows.Scan(&kind, &ref, &at, &noteID); err != nil {
				rows.Close()
				return err
			}
			src, ok := noteOf[noteID]
			if !ok {
				continue
			}
			dst, err := s.Upsert(citedKind(kind), ref, ref, "")
			if err != nil {
				rows.Close()
				return err
			}
			if _, err := s.Assert(src, "cites", dst, ep, "session", AssertOpts{ValidFrom: at, ObservedAt: at, Claim: src.Title + " cites " + ref}); err != nil {
				rows.Close()
				return err
			}
			r.Cites++
		}
		rows.Close()
	}

	// command_history → command entities (key = its id), repo entities,
	// session → touches → repo edges. Commands themselves are kept as
	// entities so a concept can cite one; the tags travel in attrs.
	rows, err = old.Query(`SELECT id, session_id, ts, repo, cmd, tags, COALESCE(exit_code, -1) FROM command_history`)
	if err == nil {
		repos := map[string]Entity{}
		sessions := map[string]Entity{}
		for rows.Next() {
			var id, ts, exit int64
			var sess, repo, cmd, tags string
			if err := rows.Scan(&id, &sess, &ts, &repo, &cmd, &tags, &exit); err != nil {
				rows.Close()
				return err
			}
			key := RepoKey(repo)
			re, ok := repos[key]
			if !ok {
				re, err = s.Upsert("repo", key, RepoName(key), "")
				if err != nil {
					rows.Close()
					return err
				}
				repos[key] = re
				r.Repos++
			}
			attrs, _ := json.Marshal(map[string]any{"tags": tags, "exit": exit, "ts": ts, "repo": key})
			ce, err := s.Upsert("command", fmt.Sprintf("old:%d", id), firstLine(cmd), string(attrs))
			if err != nil {
				rows.Close()
				return err
			}
			r.Commands++
			se, ok := sessions[sess]
			if !ok {
				se, err = s.Upsert("session", "old:"+sess, "", "")
				if err != nil {
					rows.Close()
					return err
				}
				sessions[sess] = se
			}
			if _, err := s.Assert(se, "touches", re, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts}); err != nil {
				rows.Close()
				return err
			}
			if _, err := s.Assert(se, "ran", ce, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts, Weight: 0.2}); err != nil {
				rows.Close()
				return err
			}
			// Ticket ids typed into commands (branch checkouts, gh pr
			// create -t "NME-12 …") link the session to the ticket.
			for _, t := range Tickets(cmd) {
				te, err := s.Upsert("ticket", t, t, "")
				if err != nil {
					rows.Close()
					return err
				}
				if _, err := s.Assert(se, "touches", te, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts}); err != nil {
					rows.Close()
					return err
				}
			}
		}
		rows.Close()
		r.Sessions += len(sessions)
	}
	return nil
}

// citedKind maps a section_citations.kind to an entity kind: urls stay
// urls, "shotcall"/repo.*-style refs are concepts, commands are commands.
func citedKind(kind string) string {
	switch kind {
	case "url":
		return "url"
	case "command", "cmd":
		return "command"
	}
	return "concept"
}

// backfillSessions makes one session entity per history file and links
// it to the repo it ran in (the meta entry's cwd, when the file has one)
// and to every ticket or PR mentioned in its inputs.
func (s *Store) backfillSessions(dir string, r *BackfillReport) error {
	files, err := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	if err != nil {
		return err
	}
	ep, err := s.Episode("backfill:history", dir)
	if err != nil {
		return err
	}
	for _, f := range files {
		id := strings.TrimSuffix(filepath.Base(f), ".jsonl")
		info, err := sessionInfo(f)
		if err != nil {
			continue
		}
		se, err := s.Upsert("session", id, info.title, "")
		if err != nil {
			return err
		}
		r.Sessions++
		ts := info.at
		if ts == 0 {
			ts = s.unix()
		}
		if info.repo != "" {
			re, err := s.Upsert("repo", info.repo, RepoName(info.repo), "")
			if err != nil {
				return err
			}
			if _, err := s.Assert(se, "touches", re, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts}); err != nil {
				return err
			}
		}
		for _, t := range info.tickets {
			te, err := s.Upsert("ticket", t, t, "")
			if err != nil {
				return err
			}
			if _, err := s.Assert(se, "touches", te, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts}); err != nil {
				return err
			}
		}
		for _, p := range info.prs {
			pe, err := s.Upsert("pr", p, p, "")
			if err != nil {
				return err
			}
			if _, err := s.Assert(se, "touches", pe, ep, "session", AssertOpts{ValidFrom: ts, ObservedAt: ts}); err != nil {
				return err
			}
		}
	}
	return nil
}

type sessInfo struct {
	title   string
	repo    string
	at      int64
	tickets []string
	prs     []string
}

// sessionInfo reads a history file: first input as the title, the
// meta entry's cwd resolved to a repo key when it is a git checkout,
// tickets and PRs from every input.
func sessionInfo(path string) (sessInfo, error) {
	var info sessInfo
	f, err := os.Open(path)
	if err != nil {
		return info, err
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 8<<20)
	seenT, seenP := map[string]bool{}, map[string]bool{}
	for sc.Scan() {
		var e struct {
			At   string         `json:"at"`
			Kind string         `json:"kind"`
			Data map[string]any `json:"data"`
		}
		if json.Unmarshal(sc.Bytes(), &e) != nil {
			continue
		}
		if info.at == 0 {
			if t, err := time.Parse(time.RFC3339Nano, e.At); err == nil {
				info.at = t.Unix()
			}
		}
		switch e.Kind {
		case "meta":
			if cwd, ok := e.Data["cwd"].(string); ok && cwd != "" {
				info.repo = repoOfDir(cwd)
			}
		case "input":
			text, _ := e.Data["text"].(string)
			if info.title == "" {
				info.title = truncate(firstLine(text), 80)
			}
			for _, t := range Tickets(text) {
				if !seenT[t] {
					seenT[t] = true
					info.tickets = append(info.tickets, t)
				}
			}
			for _, p := range PRs(text) {
				if !seenP[p] {
					seenP[p] = true
					info.prs = append(info.prs, p)
				}
			}
		}
	}
	return info, sc.Err()
}

// repoOfDir is the repo key of a directory: its git origin when it is
// inside a checkout, else the directory path (the old command_history
// convention).
func repoOfDir(dir string) string {
	if origin := gitOrigin(dir); origin != "" {
		return RepoKey(origin)
	}
	return dir
}

func firstLine(s string) string {
	if before, _, ok := strings.Cut(s, "\n"); ok {
		return before
	}
	return s
}
