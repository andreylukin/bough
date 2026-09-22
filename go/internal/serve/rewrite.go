package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

/*
 * The Rewrite page: a person retypes an AI-written document one sentence
 * at a time, in their own words. The model never writes into the text.
 * For each sentence it answers three things — the claim the sentence
 * makes, the phrases that read as machine prose, and what to do with it
 * — and the page shows that beside the box the person types in.
 *
 * Notes come from the config's small model, mounted here once, apart
 * from any session: a note per sentence is too chatty for a headless
 * run per call, and it must not land in history.
 */

// RewriteNote is what the model says about one sentence.
type RewriteNote struct {
	Claim  string   `json:"claim"`
	Tells  []string `json:"tells"`
	Advice string   `json:"advice"`
}

const rewriteSystem = `You help a person rewrite a document that an AI drafted, sentence by sentence, in their own words. You never rewrite for them. For the sentence given, answer ONLY a JSON object:
{"claim": "...", "tells": ["..."], "advice": "..."}
- claim: the single concrete thing the sentence asserts, in plain words, under 20 words. If it asserts nothing, say "nothing: filler".
- tells: 0-4 exact substrings of the sentence that read as machine prose: throat-clearing openers (Specifically, Furthermore, In this document), abstract noun pairs standing in for a number, hedges, "not X but Y", stacked adjectives, "leverage", "robust", "comprehensive", "seamless". Copy them verbatim.
- advice: one sentence, imperative, telling the person what to do: keep it and add what is missing, cut it (say why), merge with the next one, or say it in half the words. Name the number, example, or decision the sentence is hiding when you can tell.
Neighbouring sentences are given for context only.`

// noter answers a note for one sentence; a field so tests skip the model.
type noter func(ctx context.Context, before, sentence, after string) (RewriteNote, error)

// rewriteModel mounts the llm rows of the home config into a context of
// their own and returns the small model. It runs once; a config without
// any llm row is an error the page can show.
func (a *API) rewriteModel() (llm.LLM, error) {
	a.noteOnce.Do(func() {
		path := filepath.Join(a.home, ".bough", "bough.yml")
		rows, err := kernel.LoadFile(path)
		if err != nil {
			a.noteErr = err
			return
		}
		var keep []kernel.Row
		for _, r := range rows {
			if strings.HasPrefix(r.Plugin, "llm-") && !r.Disabled {
				keep = append(keep, r)
			}
		}
		if len(keep) == 0 {
			a.noteErr = fmt.Errorf("serve: rewrite: no llm row in %s", path)
			return
		}
		ctx := kernel.NewContext()
		ctx.Provide("ui-mode", "headless")
		if err := ctx.Reconcile(keep); err != nil {
			a.noteErr = fmt.Errorf("serve: rewrite: mount llm: %w", err)
			return
		}
		l, _ := llm.Small(ctx)
		if l == nil {
			a.noteErr = fmt.Errorf("serve: rewrite: no llm service mounted from %s", path)
			return
		}
		a.noteLLM = l
	})
	return a.noteLLM, a.noteErr
}

// modelNote asks the small model and parses its JSON, tolerating a fence
// around it. A reply that is not JSON becomes a note whose advice is the
// reply, so the person still sees something rather than an error.
func (a *API) modelNote(ctx context.Context, before, sentence, after string) (RewriteNote, error) {
	l, err := a.rewriteModel()
	if err != nil {
		return RewriteNote{}, err
	}
	var b strings.Builder
	if before != "" {
		fmt.Fprintf(&b, "Before: %s\n", before)
	}
	fmt.Fprintf(&b, "Sentence: %s\n", sentence)
	if after != "" {
		fmt.Fprintf(&b, "After: %s\n", after)
	}
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	reply, err := l.Complete(ctx, rewriteSystem, []llm.Message{{Role: "user", Content: b.String()}})
	if err != nil {
		return RewriteNote{}, err
	}
	return parseNote(reply), nil
}

var jsonObjectRE = regexp.MustCompile(`(?s)\{.*\}`)

func parseNote(reply string) RewriteNote {
	var n RewriteNote
	if m := jsonObjectRE.FindString(reply); m != "" && json.Unmarshal([]byte(m), &n) == nil && (n.Claim != "" || n.Advice != "") {
		if n.Tells == nil {
			n.Tells = []string{}
		}
		return n
	}
	return RewriteNote{Tells: []string{}, Advice: strings.TrimSpace(reply)}
}

// rewriteNote is POST /api/rewrite/note: {sentence, before, after} → the note.
func (a *API) rewriteNote(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Before   string `json:"before"`
		Sentence string `json:"sentence"`
		After    string `json:"after"`
	}
	if !decode(w, r, &body) {
		return
	}
	if strings.TrimSpace(body.Sentence) == "" {
		writeErr(w, http.StatusBadRequest, errors.New("serve: rewrite: sentence is empty"))
		return
	}
	note, err := a.note(r.Context(), body.Before, body.Sentence, body.After)
	if err != nil {
		writeErr(w, http.StatusBadGateway, err)
		return
	}
	writeJSON(w, http.StatusOK, note)
}

// --- Notion ---------------------------------------------------------

// notionVersion is the API version this reader speaks.
const notionVersion = "2022-06-28"

var notionIDRE = regexp.MustCompile(`([0-9a-f]{32})|([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})`)

// notionPageID is the page id in a notion.so / notion.site link: the last
// 32 hex digits of the path (a ?p= query names a peeked page over it).
func notionPageID(raw string) (string, bool) {
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || u.Host == "" {
		return "", false
	}
	h := strings.ToLower(u.Host)
	if !(h == "notion.so" || strings.HasSuffix(h, ".notion.so") || strings.HasSuffix(h, ".notion.site")) {
		return "", false
	}
	if p := u.Query().Get("p"); p != "" {
		if m := notionIDRE.FindString(strings.ToLower(p)); m != "" {
			return strings.ReplaceAll(m, "-", ""), true
		}
	}
	all := notionIDRE.FindAllString(strings.ToLower(u.Path), -1)
	if len(all) == 0 {
		return "", false
	}
	return strings.ReplaceAll(all[len(all)-1], "-", ""), true
}

// rewriteFetch is POST /api/rewrite/fetch: {url} → {title, text}. Only
// Notion links are read; the text is Markdown the page splits like a paste.
func (a *API) rewriteFetch(w http.ResponseWriter, r *http.Request) {
	var body struct {
		URL string `json:"url"`
	}
	if !decode(w, r, &body) {
		return
	}
	id, ok := notionPageID(body.URL)
	if !ok {
		writeErr(w, http.StatusBadRequest, errors.New("serve: rewrite: not a Notion page link; paste the text instead"))
		return
	}
	token := a.getenv("NOTION_TOKEN")
	if token == "" {
		writeErr(w, http.StatusFailedDependency, errors.New("serve: rewrite: NOTION_TOKEN is not set in ~/.bough/env; create an internal integration at notion.so/my-integrations, share the page with it, and restart serve"))
		return
	}
	n := notionClient{base: a.notionBase, token: token, http: &http.Client{Timeout: 30 * time.Second}}
	title, err := n.title(r.Context(), id)
	if err != nil {
		writeErr(w, http.StatusBadGateway, err)
		return
	}
	var out strings.Builder
	if err := n.blocks(r.Context(), id, 0, &out); err != nil {
		writeErr(w, http.StatusBadGateway, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"title": title, "text": strings.TrimSpace(out.String()) + "\n"})
}

type notionClient struct {
	base  string // "" = https://api.notion.com
	token string
	http  *http.Client
}

func (n notionClient) get(ctx context.Context, path string, v any) error {
	base := n.base
	if base == "" {
		base = "https://api.notion.com"
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, base+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+n.token)
	req.Header.Set("Notion-Version", notionVersion)
	resp, err := n.http.Do(req)
	if err != nil {
		return fmt.Errorf("serve: rewrite: notion: %w", err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if resp.StatusCode != http.StatusOK {
		var e struct {
			Message string `json:"message"`
		}
		_ = json.Unmarshal(raw, &e)
		if e.Message == "" {
			e.Message = resp.Status
		}
		return fmt.Errorf("serve: rewrite: notion: %s", e.Message)
	}
	return json.Unmarshal(raw, v)
}

type notionRich struct {
	Plain string `json:"plain_text"`
	Href  string `json:"href"`
	Ann   struct {
		Bold bool `json:"bold"`
		Code bool `json:"code"`
	} `json:"annotations"`
}

type notionBlock struct {
	ID          string `json:"id"`
	Type        string `json:"type"`
	HasChildren bool   `json:"has_children"`
	// Every text-bearing block type puts its rich_text under a key
	// named after the type; the raw map is read by that key.
	Raw map[string]json.RawMessage `json:"-"`
}

func (n notionClient) title(ctx context.Context, id string) (string, error) {
	var page struct {
		Properties map[string]struct {
			Type  string       `json:"type"`
			Title []notionRich `json:"title"`
		} `json:"properties"`
	}
	if err := n.get(ctx, "/v1/pages/"+id, &page); err != nil {
		return "", err
	}
	for _, p := range page.Properties {
		if p.Type == "title" {
			return richText(p.Title), nil
		}
	}
	return "", nil
}

func richText(rs []notionRich) string {
	var b strings.Builder
	for _, r := range rs {
		t := r.Plain
		switch {
		case r.Ann.Code:
			t = "`" + t + "`"
		case r.Href != "":
			t = "[" + t + "](" + r.Href + ")"
		case r.Ann.Bold:
			t = "**" + t + "**"
		}
		b.WriteString(t)
	}
	return b.String()
}

// blocks appends the children of id as Markdown, depth-first, with
// list items indented by depth. Unknown block types are skipped.
func (n notionClient) blocks(ctx context.Context, id string, depth int, out *strings.Builder) error {
	cursor := ""
	for {
		path := "/v1/blocks/" + id + "/children?page_size=100"
		if cursor != "" {
			path += "&start_cursor=" + url.QueryEscape(cursor)
		}
		var page struct {
			Results    []json.RawMessage `json:"results"`
			HasMore    bool              `json:"has_more"`
			NextCursor string            `json:"next_cursor"`
		}
		if err := n.get(ctx, path, &page); err != nil {
			return err
		}
		for _, raw := range page.Results {
			var b notionBlock
			if err := json.Unmarshal(raw, &b); err != nil {
				continue
			}
			_ = json.Unmarshal(raw, &b.Raw)
			var body struct {
				RichText []notionRich `json:"rich_text"`
				Language string       `json:"language"`
				Checked  bool         `json:"checked"`
			}
			if v, ok := b.Raw[b.Type]; ok {
				_ = json.Unmarshal(v, &body)
			}
			text := richText(body.RichText)
			indent := strings.Repeat("  ", depth)
			switch b.Type {
			case "heading_1":
				fmt.Fprintf(out, "\n# %s\n\n", text)
			case "heading_2":
				fmt.Fprintf(out, "\n## %s\n\n", text)
			case "heading_3":
				fmt.Fprintf(out, "\n### %s\n\n", text)
			case "paragraph":
				if text != "" {
					fmt.Fprintf(out, "%s%s\n\n", indent, text)
				}
			case "bulleted_list_item", "toggle":
				fmt.Fprintf(out, "%s- %s\n", indent, text)
			case "numbered_list_item":
				fmt.Fprintf(out, "%s1. %s\n", indent, text)
			case "to_do":
				box := "[ ]"
				if body.Checked {
					box = "[x]"
				}
				fmt.Fprintf(out, "%s- %s %s\n", indent, box, text)
			case "quote", "callout":
				fmt.Fprintf(out, "%s> %s\n\n", indent, text)
			case "code":
				fmt.Fprintf(out, "```%s\n%s\n```\n\n", body.Language, richPlain(body.RichText))
			case "divider":
				out.WriteString("\n---\n\n")
			}
			if b.HasChildren && b.Type != "child_page" && b.Type != "child_database" {
				if err := n.blocks(ctx, b.ID, depth+1, out); err != nil {
					return err
				}
			}
		}
		if !page.HasMore || page.NextCursor == "" {
			return nil
		}
		cursor = page.NextCursor
	}
}

func richPlain(rs []notionRich) string {
	var b strings.Builder
	for _, r := range rs {
		b.WriteString(r.Plain)
	}
	return b.String()
}

// noteState is the lazily mounted model behind rewriteModel.
type noteState struct {
	noteOnce sync.Once
	noteLLM  llm.LLM
	noteErr  error
}
