package prwatch

// GitHub, through gh. Everything the watcher reads comes from a few
// gh calls behind a runner func, so the policy is testable with canned
// output and the subagent gets the same facts the watcher saw.

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

// runner runs gh with args in dir and returns stdout.
type runner func(ctx context.Context, dir string, args ...string) (string, error)

func ghRunner(ctx context.Context, dir string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "gh", args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("gh %s: %w: %s", strings.Join(args[:min(2, len(args))], " "), err, firstLine(string(out)))
	}
	return string(out), nil
}

// runCmd runs a command in dir and returns its combined output.
func runCmd(ctx context.Context, dir, name string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	return string(out), err
}

func firstLine(s string) string {
	s = strings.TrimSpace(s)
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = s[:i]
	}
	return s
}

// PR is one open pull request with what the watcher may act on.
type PR struct {
	Owner    string
	Name     string
	Number   int
	Title    string
	URL      string
	Branch   string
	HeadSHA  string
	Updated  time.Time
	Threads  []Thread  // review threads (inline comments)
	Comments []Comment // conversation comments on the PR
	Checks   []Check   // CI
}

// Thread is a review thread: the first comment anchors it, the rest
// are replies.
type Thread struct {
	ID       string // GraphQL node id, what resolveReviewThread takes
	Resolved bool
	Path     string
	Line     int
	Comments []Comment
}

// Comment is one comment, review or conversation.
type Comment struct {
	ID     string // REST id for review comments (reply target), node id for conversation comments
	Author string
	Body   string
	At     time.Time
}

// Check is one CI check on the head commit.
type Check struct {
	Name  string
	State string // SUCCESS, FAILURE, ERROR, CANCELLED, TIMED_OUT, PENDING, IN_PROGRESS, QUEUED, SKIPPED, NEUTRAL, or ""
	Link  string
}

// Failed reports a check that ended badly.
func (c Check) Failed() bool {
	switch strings.ToUpper(c.State) {
	case "FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "STARTUP_FAILURE", "ACTION_REQUIRED":
		return true
	}
	return false
}

// Pending reports a check that has not finished.
func (c Check) Pending() bool {
	switch strings.ToUpper(c.State) {
	case "PENDING", "IN_PROGRESS", "QUEUED", "WAITING", "REQUESTED", "":
		return true
	}
	return false
}

// listPRs is the open PRs by the given authors across GitHub (gh
// search), updated since `since`. The session's directory does not
// matter: the watcher finds the work wherever it is.
func listPRs(ctx context.Context, run runner, dir string, authors []string, since time.Time, limit int) ([]PR, error) {
	var out []PR
	seen := map[string]bool{}
	for _, a := range authors {
		s, err := run(ctx, dir, "search", "prs", "--state", "open", "--author", a, "--limit", fmt.Sprint(limit),
			"--sort", "updated", "--json", "number,title,url,repository,updatedAt")
		if err != nil {
			return nil, err
		}
		var rows []struct {
			Number     int    `json:"number"`
			Title      string `json:"title"`
			URL        string `json:"url"`
			Repository struct {
				NameWithOwner string `json:"nameWithOwner"`
			} `json:"repository"`
			UpdatedAt time.Time `json:"updatedAt"`
		}
		if err := json.Unmarshal([]byte(s), &rows); err != nil {
			return nil, fmt.Errorf("gh search prs: %w", err)
		}
		for _, r := range rows {
			key := fmt.Sprintf("%s#%d", r.Repository.NameWithOwner, r.Number)
			if seen[key] || r.UpdatedAt.Before(since) {
				continue
			}
			owner, name, ok := strings.Cut(r.Repository.NameWithOwner, "/")
			if !ok {
				continue
			}
			seen[key] = true
			out = append(out, PR{Owner: owner, Name: name, Number: r.Number, Title: r.Title, URL: r.URL, Updated: r.UpdatedAt})
		}
	}
	return out, nil
}

const threadsQuery = `query($owner:String!,$name:String!,$n:Int!){
  repository(owner:$owner,name:$name){ pullRequest(number:$n){
    headRefOid headRefName
    reviewThreads(first:100){ nodes{ id isResolved path line comments(first:50){ nodes{ databaseId author{login} body createdAt } } } }
    comments(first:100){ nodes{ id author{login} body createdAt } }
    statusCheckRollup: commits(last:1){ nodes{ commit{ statusCheckRollup{ contexts(first:50){ nodes{
      __typename
      ... on CheckRun { name conclusion status detailsUrl }
      ... on StatusContext { context state targetUrl } } } } } } }
  } } }`

// fill loads the head, threads, comments and checks for pr.
func fill(ctx context.Context, run runner, dir string, pr *PR) error {
	s, err := run(ctx, dir, "api", "graphql", "-f", "query="+threadsQuery, "-F", "owner="+pr.Owner, "-F", "name="+pr.Name, "-F", "n="+fmt.Sprint(pr.Number))
	if err != nil {
		return err
	}
	var resp struct {
		Data struct {
			Repository struct {
				PullRequest struct {
					HeadRefOid    string `json:"headRefOid"`
					HeadRefName   string `json:"headRefName"`
					ReviewThreads struct {
						Nodes []struct {
							ID         string `json:"id"`
							IsResolved bool   `json:"isResolved"`
							Path       string `json:"path"`
							Line       int    `json:"line"`
							Comments   struct {
								Nodes []struct {
									DatabaseID int64                  `json:"databaseId"`
									Author     struct{ Login string } `json:"author"`
									Body       string                 `json:"body"`
									CreatedAt  time.Time              `json:"createdAt"`
								} `json:"nodes"`
							} `json:"comments"`
						} `json:"nodes"`
					} `json:"reviewThreads"`
					Comments struct {
						Nodes []struct {
							ID        string                 `json:"id"`
							Author    struct{ Login string } `json:"author"`
							Body      string                 `json:"body"`
							CreatedAt time.Time              `json:"createdAt"`
						} `json:"nodes"`
					} `json:"comments"`
					Rollup struct {
						Nodes []struct {
							Commit struct {
								StatusCheckRollup struct {
									Contexts struct {
										Nodes []struct {
											Typename   string `json:"__typename"`
											Name       string `json:"name"`
											Conclusion string `json:"conclusion"`
											Status     string `json:"status"`
											DetailsURL string `json:"detailsUrl"`
											Context    string `json:"context"`
											State      string `json:"state"`
											TargetURL  string `json:"targetUrl"`
										} `json:"nodes"`
									} `json:"contexts"`
								} `json:"statusCheckRollup"`
							} `json:"commit"`
						} `json:"nodes"`
					} `json:"statusCheckRollup"`
				} `json:"pullRequest"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal([]byte(s), &resp); err != nil {
		return fmt.Errorf("gh api graphql: %w", err)
	}
	p := resp.Data.Repository.PullRequest
	if p.HeadRefOid != "" {
		pr.HeadSHA = p.HeadRefOid
	}
	if p.HeadRefName != "" {
		pr.Branch = p.HeadRefName
	}
	pr.Threads = nil
	for _, t := range p.ReviewThreads.Nodes {
		th := Thread{ID: t.ID, Resolved: t.IsResolved, Path: t.Path, Line: t.Line}
		for _, c := range t.Comments.Nodes {
			th.Comments = append(th.Comments, Comment{ID: fmt.Sprint(c.DatabaseID), Author: c.Author.Login, Body: c.Body, At: c.CreatedAt})
		}
		pr.Threads = append(pr.Threads, th)
	}
	pr.Comments = nil
	for _, c := range p.Comments.Nodes {
		pr.Comments = append(pr.Comments, Comment{ID: c.ID, Author: c.Author.Login, Body: c.Body, At: c.CreatedAt})
	}
	pr.Checks = nil
	for _, n := range p.Rollup.Nodes {
		for _, c := range n.Commit.StatusCheckRollup.Contexts.Nodes {
			if c.Typename == "CheckRun" {
				state := c.Conclusion
				if state == "" {
					state = c.Status
				}
				pr.Checks = append(pr.Checks, Check{Name: c.Name, State: state, Link: c.DetailsURL})
			} else {
				pr.Checks = append(pr.Checks, Check{Name: c.Context, State: c.State, Link: c.TargetURL})
			}
		}
	}
	return nil
}

// whoami is the gh login, so the watcher never answers itself.
func whoami(ctx context.Context, run runner, dir string) (string, error) {
	s, err := run(ctx, dir, "api", "user", "--jq", ".login")
	return strings.TrimSpace(s), err
}
