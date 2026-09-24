//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"image/png"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/attachments_paste.fizz against a real serve: one web session's
// composer pasting long text, attaching an image or a file, sending,
// steering and queueing them.
//
// Most of the spec's fields are the page's own (the draft's tag,
// can_send, viewing, other), so the adapter plays the page: it keeps the
// draft, the slots (images[]), the pastes and the upload count the way
// Thread does, and applies Thread's rules to them (take/attach, send's
// lost-tag check, enqueue, expand, foldPastes). Everything the server
// decides is read off the server: an upload is a real POST to
// /api/attachments or /api/sessions/{id}/files whose answer fills the
// slot or leaves it "", running is the row, a sent message has to land
// in the history, Edit folds the recorded input back, and served is
// what GET /api/attachments answers for the sent path. The browser stage
// reads the page's fields off the DOM, where the page can be wrong in
// ways this transcription cannot see (the remount during an upload).
type attachmentsPasteAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id, otherID string
	ids         []string
	drafts      map[string]*apComposer // per session id, as localStorage keys them
	viewing     bool

	n      int      // turn and paste names are unique across walks
	held   string   // the control turn the running turn holds, "" when none
	given  []string // texts handed to the server this turn and not yet seen landed
	queued []string // the page's queue (sessionStorage), flushed after the turn
	last   string   // the last message the page gave up (sent, steered or queued)

	ran map[string]int

	// uploadFailOK is the deliberate bug TestAttachmentsPasteCatchesWrongAdapter
	// injects: UploadFail uploads successfully.
	uploadFailOK bool
}

// apComposer is one Thread's composer: the draft, its slots and pastes
// (bough:draft-atts:<id>) and the uploads in flight.
type apComposer struct {
	draft     string
	images    []string // a slot per [Image #N] / [File #N]; "" until its upload lands
	pastes    []string
	uploading int
	pend      int  // the slot the upload in flight fills, -1 when none
	pendFile  bool // that upload goes to the session's files
}

func newAttachmentsPasteAdapter(t *testing.T) *attachmentsPasteAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &attachmentsPasteAdapter{t: t, s: s, dir: control.Dir(s.Home), drafts: map[string]*apComposer{}}
	ctx, cancel := actionCtx()
	defer cancel()
	// The session the person switches to on Leave; its composer must
	// never show this one's draft.
	row, err := s.CreateSession(ctx, s.Dir(t, "other"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.otherID = row.ID
	a.drafts[row.ID] = &apComposer{pend: -1}
	return a
}

func (a *attachmentsPasteAdapter) on(action string, enabled bool) bool {
	if !a.gate.pass(enabled) {
		return false
	}
	if a.ran == nil {
		a.ran = map[string]int{}
	}
	a.ran[action]++
	return true
}

func (a *attachmentsPasteAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

func (a *attachmentsPasteAdapter) c() *apComposer { return a.drafts[a.id] }

// Init opens a fresh idle session in the same serve, viewed, with an
// empty composer.
func (a *attachmentsPasteAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.drafts[a.id] = &apComposer{pend: -1}
	a.viewing, a.held, a.given, a.queued, a.last = true, "", nil, nil, ""
	a.gate.reset()
	return nil
}

// Cleanup ends a turn the walk left held; the page's queue dies with the
// walk.
func (a *attachmentsPasteAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	a.queued = nil
	return a.settle()
}

func (a *attachmentsPasteAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Composer", Index: 0}: a}, nil
}

var (
	apTagRe    = regexp.MustCompile(`\[(Image|File) #(\d+)\]`)
	apPasteTag = regexp.MustCompile(`\[Pasted text #(\d+) \+(\d+) lines\]`)
	apSentRe   = regexp.MustCompile(`\[(Image|File) #\d+: ([^\]]+)\]`)
	apWrapped  = regexp.MustCompile(`<pasted-text lines="(\d+)">\n([\s\S]*?)\n</pasted-text>`)
)

// tag names the draft's one tag the way the spec does.
func (c *apComposer) tag() string {
	if apPasteTag.MatchString(c.draft) {
		return "paste"
	}
	m := apTagRe.FindStringSubmatch(c.draft)
	if m == nil {
		return "none"
	}
	i, _ := strconv.Atoi(m[2])
	kind := strings.ToLower(m[1])
	switch {
	case i-1 < len(c.images) && c.images[i-1] != "":
		return kind
	case c.uploading > 0 && c.pend == i-1:
		return "up_" + kind
	default:
		return "failed"
	}
}

// canSend is the Send button: a draft and no upload in flight.
func (c *apComposer) canSend() bool { return strings.TrimSpace(c.draft) != "" && c.uploading == 0 }

// apSent classifies a message as the spec's sent: what its tag became.
// A bare tag in it is a placeholder, whatever else it carries.
func apSent(text string) string {
	switch {
	case text == "":
		return "none"
	case apTagRe.MatchString(text) || apPasteTag.MatchString(text):
		return "placeholder"
	case apSentRe.MatchString(text):
		return strings.ToLower(apSentRe.FindStringSubmatch(text)[1])
	case apWrapped.MatchString(text):
		return "paste"
	}
	return "none"
}

// GetState is the Composer role: the page's fields from the adapter's
// composer, running off the row and served off GET /api/attachments.
func (a *attachmentsPasteAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	served := "none"
	if m := apSentRe.FindStringSubmatch(a.last); m != nil {
		if served, err = a.served(ctx, m[2]); err != nil {
			return nil, err
		}
	}
	c := a.c()
	return map[string]any{
		"tag":      c.tag(),
		"can_send": c.canSend(),
		"sent":     apSent(a.last),
		"served":   served,
		"running":  row.Status == serve.StatusRunning,
		"viewing":  a.viewing,
		"other":    a.drafts[a.otherID].tag(),
	}, nil
}

// served is what the transcript's <img> gets for path: the image, or a
// refusal (anything else under ~/.bough is not the page's to show).
func (a *attachmentsPasteAdapter) served(ctx context.Context, path string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+"/api/attachments?path="+url.QueryEscape(path), nil)
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	switch {
	case resp.StatusCode == http.StatusOK && strings.HasPrefix(resp.Header.Get("Content-Type"), "image/"):
		return "image", nil
	case resp.StatusCode == http.StatusNotFound:
		return "refused", nil
	case resp.StatusCode == http.StatusOK:
		return "file", nil // served, and not as an image: the bug the spec forbids
	}
	return "", fmt.Errorf("GET /api/attachments?path=%s: %d %s", path, resp.StatusCode, body)
}

// post is one upload the way api.attach / api.attachFile make it.
func (a *attachmentsPasteAdapter) post(path, contentType string, body io.Reader) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+path, body)
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", contentType)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return "", &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	var out struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal(raw, &out); err != nil || out.Path == "" {
		return "", fmt.Errorf("POST %s: no path in %s", path, raw)
	}
	return out.Path, nil
}

var apPNG = func() []byte {
	var b bytes.Buffer
	png.Encode(&b, image.NewGray(image.Rect(0, 0, 1, 1)))
	return b.Bytes()
}()

// insert is Thread's insert at the end of the draft.
func (c *apComposer) insert(s string) { c.draft += s }

func (a *attachmentsPasteAdapter) PasteLong() error {
	c := a.c()
	if !a.on("PasteLong", a.viewing && c.tag() == "none") {
		return nil
	}
	// Over 12 lines: take() keeps it as a tag instead of inserting it.
	lines := make([]string, 20)
	for i := range lines {
		lines[i] = fmt.Sprintf("%s line %d", a.name("paste"), i)
	}
	c.pastes = append(c.pastes, strings.Join(lines, "\n"))
	c.insert(fmt.Sprintf("[Pasted text #%d +%d lines] ", len(c.pastes), len(lines)))
	return nil
}

// attach is Thread's attach up to the await: the tag lands with an
// empty slot and the upload counts as in flight.
func (a *attachmentsPasteAdapter) attach(file bool) {
	c := a.c()
	c.images = append(c.images, "")
	slot := len(c.images) - 1
	kind := "Image"
	if file {
		kind = "File"
	}
	c.insert(fmt.Sprintf("[%s #%d] ", kind, slot+1))
	c.uploading++
	c.pend, c.pendFile = slot, file
}

func (a *attachmentsPasteAdapter) AttachImage() error {
	if a.on("AttachImage", a.viewing && a.c().tag() == "none") {
		a.attach(false)
	}
	return nil
}

func (a *attachmentsPasteAdapter) AttachFile() error {
	if a.on("AttachFile", a.viewing && a.c().tag() == "none") {
		a.attach(true)
	}
	return nil
}

func (a *attachmentsPasteAdapter) uploadingNow() bool {
	t := a.c().tag()
	return t == "up_image" || t == "up_file"
}

// UploadOk is the upload answering: the slot takes the server's path.
func (a *attachmentsPasteAdapter) UploadOk() error {
	if !a.on("UploadOk", a.uploadingNow()) {
		return nil
	}
	return a.upload(true)
}

// UploadFail is the upload refused: an empty image (400) or a file over
// the 64 MB cap (413). The slot stays "".
func (a *attachmentsPasteAdapter) UploadFail() error {
	if !a.on("UploadFail", a.uploadingNow()) {
		return nil
	}
	return a.upload(a.uploadFailOK)
}

func (a *attachmentsPasteAdapter) upload(ok bool) error {
	c := a.c()
	var (
		path string
		err  error
	)
	switch {
	case !c.pendFile && ok:
		path, err = a.post("/api/attachments", "image/png", bytes.NewReader(apPNG))
	case !c.pendFile:
		path, err = a.post("/api/attachments", "image/png", strings.NewReader(""))
	case ok:
		path, err = a.post("/api/sessions/"+url.PathEscape(a.id)+"/files?name=notes.txt", "text/plain", strings.NewReader("notes\n"))
	default:
		path, err = a.post("/api/sessions/"+url.PathEscape(a.id)+"/files?name=big.bin", "application/octet-stream",
			io.LimitReader(apZeros{}, 64<<20+1))
	}
	c.uploading--
	slot := c.pend
	c.pend = -1
	if ok {
		if err != nil {
			return fmt.Errorf("upload: %w", err)
		}
		c.images[slot] = path
		return nil
	}
	if err == nil {
		// The spec's UploadFail: a refused upload that was stored anyway
		// would be a server bug. Keep the slot empty like the page does
		// on an error, but say so.
		return fmt.Errorf("upload the server should refuse was stored at %s", path)
	}
	var apiErr *servetest.APIError
	if errors.As(err, &apiErr) || strings.Contains(err.Error(), "broken pipe") || strings.Contains(err.Error(), "reset") {
		return nil
	}
	return fmt.Errorf("upload: %w", err)
}

type apZeros struct{}

func (apZeros) Read(p []byte) (int, error) { clear(p); return len(p), nil }

// DeleteTag removes the tag from the draft; its content goes with it.
func (a *attachmentsPasteAdapter) DeleteTag() error {
	c := a.c()
	t := c.tag()
	if !a.on("DeleteTag", a.viewing && (t == "paste" || t == "image" || t == "file" || t == "failed")) {
		return nil
	}
	c.draft = strings.TrimSpace(apPasteTag.ReplaceAllString(apTagRe.ReplaceAllString(c.draft, ""), ""))
	return nil
}

// expand is Thread's expand: a tag whose content is held becomes it; a
// tag whose content is missing stays as it is.
func (c *apComposer) expand(t string) string {
	t = apTagRe.ReplaceAllStringFunc(t, func(m string) string {
		s := apTagRe.FindStringSubmatch(m)
		i, _ := strconv.Atoi(s[2])
		if i-1 < len(c.images) && c.images[i-1] != "" {
			return fmt.Sprintf("[%s #%d: %s]", s[1], i, c.images[i-1])
		}
		return m
	})
	return apPasteTag.ReplaceAllStringFunc(t, func(m string) string {
		s := apPasteTag.FindStringSubmatch(m)
		i, _ := strconv.Atoi(s[1])
		if i-1 >= len(c.pastes) {
			return m
		}
		return fmt.Sprintf("<pasted-text lines=%q>\n%s\n</pasted-text>", s[2], c.pastes[i-1])
	})
}

// lost is send()'s check: the tags whose content is gone.
func (c *apComposer) lost(t string) bool {
	for _, s := range apTagRe.FindAllStringSubmatch(t, -1) {
		i, _ := strconv.Atoi(s[2])
		if i-1 >= len(c.images) || c.images[i-1] == "" {
			return true
		}
	}
	for _, s := range apPasteTag.FindAllStringSubmatch(t, -1) {
		i, _ := strconv.Atoi(s[1])
		if i-1 >= len(c.pastes) {
			return true
		}
	}
	return false
}

// take is what send() and enqueue() do with the draft once it passes:
// clear it and hand back the expanded text.
func (c *apComposer) take() string {
	t := strings.TrimSpace(c.draft)
	c.draft = ""
	full := c.expand(t)
	c.pastes, c.images = nil, nil
	return full
}

func (a *attachmentsPasteAdapter) running() (bool, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	return row.Status == serve.StatusRunning, err
}

// Send is Enter: send() refuses a lost tag, else deliver() either starts
// a turn or steers the running one.
func (a *attachmentsPasteAdapter) Send() error {
	c := a.c()
	if !a.on("Send", a.viewing && c.canSend()) {
		return nil
	}
	if c.lost(strings.TrimSpace(c.draft)) {
		return nil // "Attachment unavailable: remove …"
	}
	full := c.take()
	a.last = full
	run, err := a.running()
	if err != nil {
		return err
	}
	if run {
		a.given = append(a.given, full)
		ctx, cancel := actionCtx()
		defer cancel()
		return a.s.Prompt(ctx, a.id, full)
	}
	return a.start(full, control.Turn{Mode: "block", Text: "ok"})
}

// start sends text as a new turn the model answers as turn says, and
// waits for its input to land and, for a held turn, for the row to run.
func (a *attachmentsPasteAdapter) start(text string, turn control.Turn) error {
	name := a.name("t")
	control.Queue(a.t, a.dir, name, turn)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if turn.Mode == "block" {
		a.held = name
		if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
			return err
		}
	}
	a.given = append(a.given, text)
	return a.landed()
}

// Queue is Cmd/Ctrl+Enter while the turn runs: enqueue() holds the
// message until the turn ends.
func (a *attachmentsPasteAdapter) Queue() error {
	c := a.c()
	if !a.on("Queue", a.viewing && a.held != "" && c.canSend()) {
		return nil
	}
	// enqueue runs send's lost-tag check; without it this walk queued
	// "[Image #1]" as text (sent: placeholder).
	if c.lost(strings.TrimSpace(c.draft)) {
		return nil
	}
	full := c.take()
	a.last = full
	a.queued = append(a.queued, full)
	return nil
}

// Finish ends the held turn; the page then flushes its queue one message
// per ended turn, each a turn the model answers at once.
func (a *attachmentsPasteAdapter) Finish() error {
	if !a.on("Finish", a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if err := a.settle(); err != nil {
		return err
	}
	for len(a.queued) > 0 {
		next := a.queued[0]
		a.queued = a.queued[1:]
		if err := a.start(next, control.Turn{Mode: "ok", Text: "ok"}); err != nil {
			return err
		}
		if err := a.settle(); err != nil {
			return err
		}
	}
	return nil
}

// settle waits for every message given this turn to land as an input
// (a steer lands at the turn's next boundary) and the turn to be over.
func (a *attachmentsPasteAdapter) settle() error {
	if err := a.landed(); err != nil {
		return err
	}
	_, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool {
		if r.Status == serve.StatusRunning {
			return false
		}
		e, err := history.Read(a.historyPath())
		if err != nil {
			return false
		}
		// Title and turn-summary entries follow a done; a done after
		// the last input is the turn over.
		for i := len(e) - 1; i >= 0; i-- {
			switch e[i].Kind {
			case "done":
				return true
			case "input":
				return false
			}
		}
		return false
	})
	if err != nil {
		e, _ := history.Read(a.historyPath())
		var kinds []string
		for _, x := range e {
			kinds = append(kinds, x.Kind)
		}
		return fmt.Errorf("%w; history kinds %v", err, kinds)
	}
	return nil
}

func (a *attachmentsPasteAdapter) historyPath() string {
	return filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")
}

// landed waits until every given text is recorded, verbatim, as an input.
func (a *attachmentsPasteAdapter) landed() error {
	if len(a.given) == 0 {
		return nil
	}
	want := a.given
	_, err := waitRow(a.s, a.id, fmt.Sprintf("%d sent message(s) to land", len(want)), func(serve.Row) bool {
		e, err := history.Read(a.historyPath())
		if err != nil {
			return false
		}
		have := map[string]int{}
		for _, x := range e {
			if x.Kind == "input" {
				have[history.Prompt(x)]++
			}
		}
		for _, w := range want {
			if have[w] == 0 {
				return false
			}
			have[w]--
		}
		return true
	})
	if err != nil {
		raw, _ := os.ReadFile(a.historyPath())
		return fmt.Errorf("%w; want %q; history:\n%s", err, want, raw)
	}
	a.given = nil
	return nil
}

// Edit is the Edit button on the message the person last sent: its
// recorded text goes back in the composer with each wrapped paste folded
// to its tag (foldPastes).
func (a *attachmentsPasteAdapter) Edit() error {
	c := a.c()
	if !a.on("Edit", a.viewing && a.held == "" && c.tag() == "none" && apSent(a.last) == "paste") {
		return nil
	}
	e, err := history.Read(a.historyPath())
	if err != nil {
		return err
	}
	for i := len(e) - 1; i >= 0; i-- {
		if e[i].Kind != "input" || history.Prompt(e[i]) != a.last {
			continue
		}
		c.draft = apWrapped.ReplaceAllStringFunc(history.Prompt(e[i]), func(m string) string {
			s := apWrapped.FindStringSubmatch(m)
			c.pastes = append(c.pastes, s[2])
			return fmt.Sprintf("[Pasted text #%d +%s lines]", len(c.pastes), s[1])
		})
		return nil
	}
	return fmt.Errorf("Edit: the sent message is not in the history")
}

func (a *attachmentsPasteAdapter) Leave() error {
	if a.on("Leave", a.viewing && a.c().tag() != "none" && a.held == "") {
		a.viewing = false
	}
	return nil
}

func (a *attachmentsPasteAdapter) Return() error {
	if a.on("Return", !a.viewing) {
		a.viewing = true
	}
	return nil
}

var attachmentsPasteActions = map[string]map[string]fmbt.ActionFunc{"Composer": {
	"PasteLong":   action((*attachmentsPasteAdapter).PasteLong),
	"AttachImage": action((*attachmentsPasteAdapter).AttachImage),
	"AttachFile":  action((*attachmentsPasteAdapter).AttachFile),
	"UploadOk":    action((*attachmentsPasteAdapter).UploadOk),
	"UploadFail":  action((*attachmentsPasteAdapter).UploadFail),
	"DeleteTag":   action((*attachmentsPasteAdapter).DeleteTag),
	"Send":        action((*attachmentsPasteAdapter).Send),
	"Queue":       action((*attachmentsPasteAdapter).Queue),
	"Finish":      action((*attachmentsPasteAdapter).Finish),
	"Edit":        action((*attachmentsPasteAdapter).Edit),
	"Leave":       action((*attachmentsPasteAdapter).Leave),
	"Return":      action((*attachmentsPasteAdapter).Return),
}}

func attachmentsPasteOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// attachmentsPasteHistory reads the abstract trace off a transcript.
// Only what was sent is recorded, so each input is put back as the steps
// that make such a message (a paste; an attach and its upload) and a
// Send; its sent and served are read off the recorded text. A message
// the page queued lands after the turn's done, which is a Send of its
// own in the model too. Refused sends, deletes, Leave/Return and Edit
// (a paste again) leave nothing the model needs.
func attachmentsPasteHistory(entries []history.Entry) []tracecheck.Step {
	f := func(k string) string { return "Composer#0." + k }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{f("tag"): "none", f("sent"): "none", f("running"): false}}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: f(action), State: state})
	}
	open := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text := history.Prompt(e)
			sent := apSent(text)
			served := map[string]string{"image": "image", "file": "refused"}[sent]
			if served == "" {
				served = "none"
			}
			switch sent {
			case "paste":
				add("PasteLong", nil)
			case "image":
				add("AttachImage", nil)
				add("UploadOk", nil)
			case "file":
				add("AttachFile", nil)
				add("UploadOk", nil)
			}
			add("Send", map[string]any{f("sent"): sent, f("served"): served, f("running"): true})
			open = true
		case "done":
			if open {
				add("Finish", map[string]any{f("running"): false})
				open = false
			}
		}
	}
	return steps
}

func init() { historyProjections["attachments_paste"] = attachmentsPasteHistory }

// The runner picks among all twelve actions at random and stops checking
// at the first disabled one, so its walks rarely get past an attach; it
// still runs, and TestAttachmentsPastePaths drives the same adapter down
// every generated path (every transition) comparing the whole role.
func TestAttachmentsPaste(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAttachmentsPasteAdapter(t)
	err := runMBT(t, "attachments_paste", a, attachmentsPasteActions, attachmentsPasteOptions())
	t.Logf("actions run past the gate: %v", a.ran)
	if err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkAttachmentsPasteHistories(t, a)
}

func TestAttachmentsPastePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAttachmentsPasteAdapter(t)
	err := walkAttachmentsPastePaths(t, a, attachmentsPasteActions["Composer"])
	t.Logf("actions run: %v", a.ran)
	if err != nil {
		t.Fatal(err)
	}
	checkAttachmentsPasteHistories(t, a)
}

// Every transcript the walks wrote is itself a path in the model.
func checkAttachmentsPasteHistories(t *testing.T, a *attachmentsPasteAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "attachments_paste"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), attachmentsPasteHistory)
	}
}

// A walk whose UploadFail stores the upload must fail: otherwise the
// green walks above prove nothing.
func TestAttachmentsPasteCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAttachmentsPasteAdapter(t)
	a.uploadFailOK = true
	err := walkAttachmentsPastePaths(t, a, attachmentsPasteActions["Composer"])
	if err == nil {
		t.Fatal("paths whose UploadFail stores the upload passed; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}

// apStutters are the spec's enabled actions that change nothing, which
// fizz leaves out of the graph (and so the generator out of paths.json):
// Send and Queue on a failed tag. They are where NoPlaceholderSent is
// decided, so the walk takes each one wherever the spec enables it and
// requires the state to stay the node's.
var apStutters = []struct {
	action  string
	enabled func(state map[string]any) bool
}{
	{"Send", func(s map[string]any) bool {
		return s["tag"] == "failed" && s["viewing"] == true && s["can_send"] == true
	}},
	{"Queue", func(s map[string]any) bool {
		return s["tag"] == "failed" && s["viewing"] == true && s["can_send"] == true && s["running"] == true
	}},
}

// walkAttachmentsPastePaths drives a down every path in
// testdata/attachments_paste/paths.json and compares its state with the
// node after Init and after each action, taking the stutter steps above
// at every node that enables them (once per distinct node). It stops at
// the first path that disagrees.
func walkAttachmentsPastePaths(t *testing.T, a *attachmentsPasteAdapter, actions map[string]fmbt.ActionFunc) error {
	const role = "Composer#0"
	raw, err := os.ReadFile(filepath.Join(filepath.Dir(specPath("attachments_paste")), "..", "testdata", "attachments_paste", "paths.json"))
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return err
	}
	stuttered := map[string]bool{}
	for pi, p := range doc.Paths {
		var names []string
		for _, st := range p.Trace {
			names = append(names, strings.TrimPrefix(st.Action, role+"."))
		}
		fail := func(i int, format string, args ...any) error {
			a.Cleanup()
			return fmt.Errorf("path %d %v, step %d (%s): %s", pi, names, i, names[i], fmt.Sprintf(format, args...))
		}
		for i, st := range p.Trace {
			if i == 0 {
				if err := a.Init(); err != nil {
					return fail(i, "%v", err)
				}
			} else {
				f, ok := actions[names[i]]
				if !ok {
					return fail(i, "no such action")
				}
				if _, err := f(a, nil); err != nil {
					return fail(i, "%v", err)
				}
			}
			want := map[string]any{}
			for k, v := range st.State {
				if field, ok := strings.CutPrefix(k, role+"."); ok {
					want[field] = v
				}
			}
			check := func(after string) error {
				got, err := a.GetState()
				if err != nil {
					return fail(i, "%s%v", after, err)
				}
				for field, v := range want {
					if !reflect.DeepEqual(got[field], v) {
						return fail(i, "%s%s: model %v, server %v (whole state %v)", after, field, v, got[field], got)
					}
				}
				return nil
			}
			if err := check(""); err != nil {
				return err
			}
			key, _ := json.Marshal(want)
			for _, s := range apStutters {
				if !s.enabled(want) || stuttered[string(key)+s.action] {
					continue
				}
				stuttered[string(key)+s.action] = true
				if _, err := actions[s.action](a, nil); err != nil {
					return fail(i, "then %s: %v", s.action, err)
				}
				if err := check("then " + s.action + ", which changes nothing: "); err != nil {
					return err
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d cleanup: %w", pi, err)
		}
	}
	if len(stuttered) == 0 {
		return fmt.Errorf("no path reached a failed tag: the stutter steps never ran")
	}
	return nil
}
