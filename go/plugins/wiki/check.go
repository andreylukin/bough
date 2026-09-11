package wiki

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

// Problem is one thing wrong with the wiki, as a line the lint agent
// (or a person) can act on.
type Problem struct {
	Page string // relative to the wiki directory
	Line int
	Msg  string
}

func (p Problem) String() string { return fmt.Sprintf("%s:%d: %s", p.Page, p.Line, p.Msg) }

// citeRE is a citation of a history entry: `<session>#<seq>` in
// backticks. It is the wiki's only link back to what happened.
var citeRE = regexp.MustCompile("`([A-Za-z0-9][A-Za-z0-9:._-]*)#(\\d+)`")

// linkRE is a relative markdown link to another page.
var linkRE = regexp.MustCompile(`\]\(([^)\s:#]+\.md)(?:#[^)]*)?\)`)

// Check verifies what can be verified without a model: every cited
// history entry exists, every relative page link resolves, and every
// page is listed in index.md. Whether a page says what its citations
// say is the lint agent's job, not this one's.
func Check(p paths) ([]Problem, error) {
	index, _ := os.ReadFile(p.index())
	seqs := map[string]map[int64]bool{} // session -> its seqs; nil when the session file is missing
	var probs []Problem
	err := filepath.WalkDir(p.wiki, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			if strings.HasPrefix(d.Name(), ".") && path != p.wiki {
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".md") {
			return nil
		}
		rel, _ := filepath.Rel(p.wiki, path)
		rel = filepath.ToSlash(rel)
		b, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		special := rel == "index.md" || rel == "log.md"
		if !special && !strings.Contains(string(index), rel) {
			probs = append(probs, Problem{rel, 1, "not listed in index.md"})
		}
		for i, line := range strings.Split(string(b), "\n") {
			if rel != "log.md" { // the log names sessions, it does not cite entries
				for _, m := range citeRE.FindAllStringSubmatch(line, -1) {
					if msg := checkCite(p, seqs, m[1], m[2]); msg != "" {
						probs = append(probs, Problem{rel, i + 1, msg})
					}
				}
			}
			for _, m := range linkRE.FindAllStringSubmatch(line, -1) {
				target := filepath.Join(filepath.Dir(path), filepath.FromSlash(m[1]))
				if _, err := os.Stat(target); err != nil {
					probs = append(probs, Problem{rel, i + 1, "broken link " + m[1]})
				}
			}
		}
		return nil
	})
	return probs, err
}

func checkCite(p paths, seqs map[string]map[int64]bool, id, seqText string) string {
	set, loaded := seqs[id]
	if !loaded {
		entries, err := history.Read(filepath.Join(p.hist, id+".jsonl"))
		if err == nil {
			set = map[int64]bool{}
			for _, e := range entries {
				set[e.Seq] = true
			}
		}
		seqs[id] = set
	}
	if set == nil {
		return "cites a session that does not exist: " + id
	}
	n, _ := strconv.ParseInt(seqText, 10, 64)
	if !set[n] {
		return fmt.Sprintf("cites entry #%d, which session %s does not have", n, id)
	}
	return ""
}
