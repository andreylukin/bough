package wiki

// The brief: topics/me/briefs/<date>.md, what the person is doing today
// across everything the agent can read — bough threads, git, GitHub,
// Slack, Linear, Notion — in the voice of a standup reply, every line
// cited. Written by a headless session running `/llm-wiki brief`, the
// way an ingest runs `/llm-wiki ingest`; the control room's Me page
// renders the result.
//
// One brief per day, frozen after that day: the file is rewritten while
// its date is today and never again, so briefs/ is a diary. The tick
// writes it during working hours, at most every half hour, and only
// once the person has written topics/me/profile.md — a brief about
// nobody is noise, and the profile is where the agent learns whose
// work it is looking at.

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	briefEvery = 30 * time.Minute // between rewrites of today's brief
	briefFrom  = 7                // the working-hours window, local time
	briefUntil = 19
)

func (p paths) me() string      { return filepath.Join(p.wiki, "topics", "me") }
func (p paths) profile() string { return filepath.Join(p.me(), "profile.md") }

// BriefPath is today's brief, relative to the wiki, for a date.
func BriefPath(day time.Time) string {
	return "topics/me/briefs/" + day.Format("2006-01-02") + ".md"
}

// briefDue says whether the tick should write today's brief now: a
// profile exists, it is a working hour, and today's brief is missing or
// older than briefEvery. force skips the hour and age checks (the Me
// page's Refresh), never the profile — there is nothing to brief without one.
func briefDue(p paths, now time.Time, force bool) (bool, string) {
	if _, err := os.Stat(p.profile()); err != nil {
		return false, "no topics/me/profile.md"
	}
	if force {
		return true, ""
	}
	if wd := now.Weekday(); wd == time.Saturday || wd == time.Sunday {
		return false, "weekend"
	}
	if h := now.Hour(); h < briefFrom || h >= briefUntil {
		return false, "outside working hours"
	}
	st, err := os.Stat(filepath.Join(p.wiki, filepath.FromSlash(BriefPath(now))))
	if err == nil && now.Sub(st.ModTime()) < briefEvery {
		return false, "brief is fresh"
	}
	return true, ""
}

// runBrief runs one headless `/llm-wiki brief` in the wiki directory,
// with the same environment an ingest gets. The caller holds the lock.
func runBrief(p paths, exe string) error {
	fmt.Printf("%s brief %s\n", time.Now().Format(time.RFC3339), BriefPath(time.Now()))
	ctx, cancel := context.WithTimeout(context.Background(), runTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, exe, "--headless")
	cmd.Dir = p.wiki // started here, so FindPending never ingests the brief's own session
	cmd.Stdin = strings.NewReader("/llm-wiki brief\n")
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	cmd.Env = append(os.Environ(),
		"BOUGH_WEB_ADDR=127.0.0.1:0",
		"BOUGH_BIN="+exe,
		"BOUGH_WRITE_ROOTS="+p.wiki,
		"PATH="+filepath.Dir(exe)+string(os.PathListSeparator)+os.Getenv("PATH"),
	)
	err := cmd.Run()
	commit(p, "brief "+time.Now().Format("2006-01-02 15:04"))
	return err
}

// Brief writes today's brief now, if there is a profile: the CLI's
// `bough wiki brief` and the Me page's Refresh.
func Brief(p paths, exe string) error {
	if err := ensureWiki(p); err != nil {
		return err
	}
	unlock, ok := tryLock(filepath.Join(p.wiki, ".ingest.lock"))
	if !ok {
		return fmt.Errorf("wiki: an ingest or brief is already running")
	}
	defer unlock()
	if err := writeSkill(p); err != nil {
		return err
	}
	if due, why := briefDue(p, time.Now(), true); !due {
		return fmt.Errorf("wiki: no brief: %s", why)
	}
	return runBrief(p, exe)
}

// Me is the brief as the control room's Me page reads it: today's brief
// when there is one, else the latest earlier day's (marked), the signals
// the last run wrote, and whether there is a profile to brief about.
type Me struct {
	Date       string `json:"date"`
	HasProfile bool   `json:"hasProfile"`
	// Path and Page are the brief shown: today's, or the latest earlier
	// one when today has none yet, in which case Stale is set.
	Path  string `json:"path,omitempty"`
	Page  *Page  `json:"page,omitempty"`
	Stale bool   `json:"stale,omitempty"`
	AsOf  string `json:"asOf,omitempty"`
	// Signals is topics/me/signals.json as written, or null.
	Signals json.RawMessage `json:"signals,omitempty"`
	// Days is every brief on disk, newest first, as dates.
	Days []string `json:"days"`
}

// Me reads the brief for the page. It never writes.
func (s *Store) Me(now time.Time) Me {
	m := Me{Date: now.Format("2006-01-02"), Days: []string{}}
	if _, err := os.Stat(s.p.profile()); err == nil {
		m.HasProfile = true
	}
	dir := filepath.Join(s.p.me(), "briefs")
	ents, _ := os.ReadDir(dir)
	for _, e := range ents {
		if name := e.Name(); strings.HasSuffix(name, ".md") && len(name) == len("2006-01-02.md") {
			m.Days = append(m.Days, strings.TrimSuffix(name, ".md"))
		}
	}
	sort.Sort(sort.Reverse(sort.StringSlice(m.Days)))
	if len(m.Days) > 0 {
		day := m.Days[0]
		m.Path = "topics/me/briefs/" + day + ".md"
		m.Stale = day != m.Date
		if pg, err := s.Page(m.Path); err == nil {
			m.Page = &pg
		}
		if st, err := os.Stat(filepath.Join(dir, day+".md")); err == nil {
			m.AsOf = st.ModTime().Format(time.RFC3339)
		}
	}
	if b, err := os.ReadFile(filepath.Join(s.p.me(), "signals.json")); err == nil && json.Valid(b) {
		m.Signals = json.RawMessage(b)
	}
	return m
}
