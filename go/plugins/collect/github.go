package collect

// GitHub through gh: my open PRs, PRs awaiting my review, and the
// open PRs the graph already holds for me (so a merge closes their
// state window). One `gh pr view` per PR gives the link truth; one
// GraphQL call counts the unresolved review threads, which is what
// "fix the Devin comments" is about.

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/andreylukin/bough/plugins/graph"
)

const prFields = "number,title,url,state,isDraft,updatedAt,author,headRefName,body,reviewRequests,reviews,reviewDecision,statusCheckRollup,mergedAt,closedAt"

type ghPR struct {
	Number         int    `json:"number"`
	Title          string `json:"title"`
	URL            string `json:"url"`
	State          string `json:"state"`
	IsDraft        bool   `json:"isDraft"`
	UpdatedAt      string `json:"updatedAt"`
	MergedAt       string `json:"mergedAt"`
	ClosedAt       string `json:"closedAt"`
	HeadRefName    string `json:"headRefName"`
	Body           string `json:"body"`
	ReviewDecision string `json:"reviewDecision"`
	Author         struct {
		Login string `json:"login"`
	} `json:"author"`
	ReviewRequests []struct {
		Login string `json:"login"`
		Name  string `json:"name"` // a team
	} `json:"reviewRequests"`
	Reviews []struct {
		State  string `json:"state"`
		Author struct {
			Login string `json:"login"`
		} `json:"author"`
	} `json:"reviews"`
	StatusCheckRollup []struct {
		Conclusion string `json:"conclusion"`
		State      string `json:"state"`
	} `json:"statusCheckRollup"`
}

// Github collects. login is my GitHub login (looked up when "").
func (r *Run) Github() Report {
	rep := Report{Source: "github"}
	out, err := r.gh("api", "user", "--jq", ".login")
	if err != nil {
		rep.Err = fmt.Errorf("gh api user: %w (gh auth login?)", err)
		return rep
	}
	login := strings.TrimSpace(string(out))
	_ = r.St.Alias(r.Me.ID, "github", login, "https://github.com/"+login)

	urls := map[string]bool{}
	for _, q := range [][]string{
		{"search", "prs", "--author=@me", "--state=open", "--json", "url", "--limit", "50"},
		{"search", "prs", "--review-requested=@me", "--state=open", "--json", "url", "--limit", "50"},
	} {
		out, err := r.gh(q...)
		if err != nil {
			rep.Err = fmt.Errorf("gh %s: %w", strings.Join(q[:3], " "), err)
			return rep
		}
		var hits []struct{ URL string }
		if err := json.Unmarshal(out, &hits); err != nil {
			rep.Err = fmt.Errorf("gh search prs: %w", err)
			return rep
		}
		for _, h := range hits {
			urls[h.URL] = true
		}
	}
	// What the graph thinks is still open of mine: a merge shows up
	// here, not in a search for open PRs.
	if w, err := r.St.WorldOf(r.Me); err == nil {
		for _, e := range w.Mine {
			if e.Kind == "pr" && e.URL != "" {
				urls[e.URL] = true
			}
		}
	}

	for url := range urls {
		out, err := r.gh("pr", "view", url, "--json", prFields)
		if err != nil {
			r.Log("github: %s: %v", url, err)
			continue
		}
		var pr ghPR
		if err := json.Unmarshal(out, &pr); err != nil {
			r.Log("github: %s: %v", url, err)
			continue
		}
		threads := r.unresolvedThreads(url)
		ents, edges, err := r.recordPR(url, pr, threads, login)
		if err != nil {
			rep.Err = err
			return rep
		}
		rep.Entities += ents
		rep.Edges += edges
	}
	return rep
}

// recordPR writes one PR's link truth and edges.
func (r *Run) recordPR(url string, pr ghPR, unresolved int, myLogin string) (ents, edges int, err error) {
	key, ok := prKeyFromURL(url)
	if !ok {
		return 0, 0, nil
	}
	at := when(pr.UpdatedAt)
	e, err := r.pr(key, pr.Title, url)
	if err != nil {
		return 0, 0, err
	}
	ents++
	st := status(pr.State)
	if st == "open" && pr.IsDraft {
		st = "draft"
	}
	if err := r.St.SetState(e, st, r.Ep, "collector", at); err != nil {
		return ents, edges, err
	}
	if _, err := r.St.SetLink(e, graph.Link{Summary: prSummary(pr, unresolved), UpdatedAt: at}); err != nil {
		return ents, edges, err
	}
	count := func(ok bool, err error) error {
		if ok {
			edges++
		}
		return err
	}
	author, err := r.person("github", pr.Author.Login, "", pr.Author.Login)
	if err != nil {
		return ents, edges, err
	}
	if err := count(r.assert(author, "authored", e, at, "")); err != nil {
		return ents, edges, err
	}
	for _, t := range graph.Tickets(pr.HeadRefName + " " + pr.Title + " " + pr.Body) {
		te, err := r.ticket(t)
		if err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(e, "implements", te, at, "")); err != nil {
			return ents, edges, err
		}
	}
	if st != "open" && st != "draft" {
		// Closed windows: nobody is awaited any more.
		return ents, edges, r.closeAwaits(e, at)
	}
	// Who it waits on: requested reviewers until they review; the
	// author once someone asked for changes or a bot thread is open.
	awaited := map[string]bool{}
	for _, rr := range pr.ReviewRequests {
		if rr.Login == "" {
			continue
		}
		p, err := r.person("github", rr.Login, "", rr.Login)
		if err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(p, "reviews", e, at, "requested")); err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(e, "awaits", p, at, "review requested")); err != nil {
			return ents, edges, err
		}
		awaited[p.Key] = true
	}
	changes := false
	for _, rv := range pr.Reviews {
		if rv.Author.Login == "" || rv.Author.Login == pr.Author.Login {
			continue
		}
		p, err := r.person("github", rv.Author.Login, "", rv.Author.Login)
		if err != nil {
			return ents, edges, err
		}
		if err := count(r.assert(p, "reviews", e, at, strings.ToLower(rv.State))); err != nil {
			return ents, edges, err
		}
		if rv.State == "CHANGES_REQUESTED" {
			changes = true
		}
	}
	if changes || unresolved > 0 {
		claim := "changes requested"
		if unresolved > 0 {
			claim = fmt.Sprintf("%d unresolved review threads", unresolved)
		}
		if err := count(r.assert(e, "awaits", author, at, claim)); err != nil {
			return ents, edges, err
		}
		awaited[author.Key] = true
	}
	// Anyone awaited last run and not now is released.
	open, err := r.St.Neighbors(e, 1, "awaits", 0)
	if err != nil {
		return ents, edges, err
	}
	for _, o := range open {
		if o.Src.ID == e.ID && !awaited[o.Dst.Key] {
			if err := r.St.Invalidate(o.ID, "no longer awaited", "collector", at); err != nil {
				return ents, edges, err
			}
		}
	}
	return ents, edges, nil
}

func (r *Run) closeAwaits(e graph.Entity, at int64) error {
	open, err := r.St.Neighbors(e, 1, "awaits", 0)
	if err != nil {
		return err
	}
	for _, o := range open {
		if o.Src.ID == e.ID {
			if err := r.St.Invalidate(o.ID, "closed", "collector", at); err != nil {
				return err
			}
		}
	}
	return nil
}

// unresolvedThreads counts open review threads (Devin, Codex, humans)
// via GraphQL; -1 when the call fails, which the summary then omits.
func (r *Run) unresolvedThreads(url string) int {
	owner, repo, n, ok := splitPRURL(url)
	if !ok {
		return -1
	}
	q := `query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100){nodes{isResolved}}}}}`
	out, err := r.gh("api", "graphql", "-f", "query="+q, "-F", "o="+owner, "-F", "r="+repo, "-F", fmt.Sprintf("n=%d", n),
		"--jq", "[.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved|not)] | length")
	if err != nil {
		return -1
	}
	var c int
	if _, err := fmt.Sscanf(strings.TrimSpace(string(out)), "%d", &c); err != nil {
		return -1
	}
	return c
}

// prSummary is the one line the world shows: CI, review decision,
// open threads.
func prSummary(pr ghPR, unresolved int) string {
	var parts []string
	if ci := ciState(pr); ci != "" {
		parts = append(parts, "ci "+ci)
	}
	if pr.ReviewDecision != "" {
		parts = append(parts, strings.ToLower(strings.ReplaceAll(pr.ReviewDecision, "_", " ")))
	}
	if unresolved > 0 {
		parts = append(parts, fmt.Sprintf("%d unresolved threads", unresolved))
	}
	if pr.HeadRefName != "" {
		parts = append(parts, "branch "+pr.HeadRefName)
	}
	return strings.Join(parts, ", ")
}

func ciState(pr ghPR) string {
	if len(pr.StatusCheckRollup) == 0 {
		return ""
	}
	pending := false
	for _, c := range pr.StatusCheckRollup {
		switch strings.ToUpper(c.Conclusion) {
		case "FAILURE", "TIMED_OUT", "CANCELLED", "ERROR":
			return "failing"
		case "":
			if strings.ToUpper(c.State) == "FAILURE" || strings.ToUpper(c.State) == "ERROR" {
				return "failing"
			}
			pending = true
		}
	}
	if pending {
		return "pending"
	}
	return "green"
}

func splitPRURL(url string) (owner, repo string, n int, ok bool) {
	parts := strings.Split(strings.TrimPrefix(strings.TrimPrefix(url, "https://"), "http://"), "/")
	if len(parts) < 5 || parts[0] != "github.com" || parts[3] != "pull" {
		return "", "", 0, false
	}
	if _, err := fmt.Sscanf(parts[4], "%d", &n); err != nil {
		return "", "", 0, false
	}
	return parts[1], parts[2], n, true
}

func prKeyFromURL(url string) (string, bool) {
	_, repo, n, ok := splitPRURL(url)
	if !ok {
		return "", false
	}
	return fmt.Sprintf("%s#%d", repo, n), true
}
