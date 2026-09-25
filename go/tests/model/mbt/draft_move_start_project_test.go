//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/draft_move_start_project.fizz against a real serve: a read-only
// local session's composer holds a draft, "Start project session" picks
// a project, and a new orb session (on the fake container runtime) is
// created with the draft moved into its composer.
//
// Most of the spec's fields are the page's own (both composers' tags,
// the localStorage keys, the view), so the adapter plays the page: it
// keeps what Thread and App's onStartProject keep in localStorage
// (bough:draft, bough:draft-atts, bough:draft-ask per session) and the
// upload in flight (the module's uploads map), and applies their rules
// to it. Everything the server decides is read off the server: whether
// the picked project asks first is failedBuild() of GET /api/projects
// (a project whose build.json says failed), the create is a real POST
// held at the child's start by llm-control (CreateOk releases it,
// CreateFail makes the child exit), moved is whether serve lists a new
// project session, an upload is a real POST /api/attachments, and sent
// is what the new session's history recorded. The browser stage reads
// the page's fields off the DOM.
const dmConfig = "- id: llm\n  plugin: llm-control\n" +
	"- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

var (
	dmImageTag  = regexp.MustCompile(`\[Image #(\d+)\]`)
	dmPasteTag  = regexp.MustCompile(`\[Pasted text #(\d+) \+(\d+) lines\]`)
	dmImageSent = regexp.MustCompile(`\[Image #\d+: [^\]]+\]`)
	dmWrapped   = regexp.MustCompile(`<pasted-text lines="\d+">\n[\s\S]*?\n</pasted-text>`)
)

// dmAtts is bough:draft-atts:<id>.
type dmAtts struct {
	pastes []string
	images []string
}

// dmUpload is one upload in flight: registered in the page's uploads
// map under owner, filling slot of that session's draft-atts.
type dmUpload struct {
	owner string
	slot  int
}

type draftMoveAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue and start hold
	gate gate

	oldID   string // the read-only local session, one for every walk
	healthy string // this walk's project that starts at once
	broken  string // this walk's project whose last build failed
	walk    int
	n       int

	// The page's localStorage, per session id, and its uploads map.
	draft   map[string]string
	atts    map[string]*dmAtts
	ask     map[string]bool
	pending *dmUpload

	view   string
	newID  string // the session the move created, "" before
	picked string // the project picked, until the create answers
	before map[string]bool
	post   chan dmCreate // the create in flight, until answered
	typed  bool          // the old draft was ever non-empty
	ids    []string      // every session a create made, for the trace check

	ran map[string]int

	// createFailStarts is the deliberate bug
	// TestDraftMoveStartProjectCatchesWrongAdapter injects: CreateFail
	// lets the held child start, so the create succeeds.
	createFailStarts bool
}

type dmCreate struct {
	row serve.Row
	err error
}

func newDraftMoveAdapter(t *testing.T) *draftMoveAdapter {
	s := servetest.Start(t, servetest.Options{Config: dmConfig})
	a := &draftMoveAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	ctx, cancel := actionCtx()
	defer cancel()
	// Outside any git checkout: the session is read-only, which is when
	// the composer offers "Start project session".
	row, err := s.CreateSession(ctx, s.Dir(t, "notes"), "")
	if err != nil {
		t.Fatal(err)
	}
	if row.Writable != "" {
		t.Fatalf("the old session is writable (%s); the composer would not offer a project session", row.Writable)
	}
	a.oldID = row.ID
	return a
}

// project makes a project through the API, as the page's New project does.
func (a *draftMoveAdapter) project(name string) (string, error) {
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": name}, &r); err != nil {
		return "", err
	}
	return r.Project.Slug, nil
}

func (a *draftMoveAdapter) api(method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: fmt.Sprintf("%s %s: %s", method, path, bytes.TrimSpace(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

func (a *draftMoveAdapter) on(action string, enabled bool) bool {
	if !a.gate.pass(enabled) {
		return false
	}
	if a.ran == nil {
		a.ran = map[string]int{}
	}
	a.ran[action]++
	return true
}

// Init starts a walk: the old session's composer empty, no new session,
// and two fresh projects. The failed one must be fresh because a walk
// that started it anyway built its image, so it asks no more. The
// healthy one must be because a create only fails at the child's start
// while it waits on the project's main booting: once main is live, serve
// answers a thread's create as soon as its process is exec'd, and a
// thread that then dies is not a failed create (see the flow's notes).
func (a *draftMoveAdapter) Init() error {
	a.walk++
	var err error
	if a.healthy, err = a.project(fmt.Sprintf("Healthy %d", a.walk)); err != nil {
		return err
	}
	slug, err := a.project(fmt.Sprintf("Broken %d", a.walk))
	if err != nil {
		return err
	}
	a.broken = slug
	img := filepath.Join(a.s.Home, ".bough", "orbs", "images", slug)
	if err := os.MkdirAll(img, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(img, "build.json"), []byte(`{"state":"failed","error":"exit status 1"}`), 0o644); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, true)
	if err != nil {
		return err
	}
	a.before = map[string]bool{}
	for _, r := range rows {
		a.before[r.ID] = true
	}
	a.draft, a.atts, a.ask, a.pending = map[string]string{}, map[string]*dmAtts{}, map[string]bool{}, nil
	a.view, a.newID, a.picked, a.post, a.typed = "old", "", "", nil, false
	a.gate.reset()
	return nil
}

// Cleanup answers a create still in flight (its child exits) and
// archives what the walk created, so a hundred walks do not leave a
// hundred children.
func (a *draftMoveAdapter) Cleanup() error {
	var errs []error
	if a.post != nil {
		control.ExitStart(a.t, a.dir)
		if r := a.collect(); r.err == nil {
			a.newID = r.row.ID
		}
	}
	control.ReleaseStart(a.t, a.dir)
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return err
	}
	for _, r := range rows {
		if a.created(r) {
			// The create answers before the child writes its history, and
			// serve archives only a session it has a history file for.
			if err := a.historyWritten(r.ID); err != nil {
				errs = append(errs, err)
				continue
			}
			if _, err := a.s.Archive(ctx, r.ID); err != nil {
				errs = append(errs, err)
			}
		}
	}
	// And the walk's projects, which stops their mains.
	for _, slug := range []string{a.healthy, a.broken} {
		if slug != "" {
			errs = append(errs, a.api(http.MethodPost, "/api/projects/"+slug+"/archive", nil, nil))
		}
	}
	return errors.Join(errs...)
}

func (a *draftMoveAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Move", Index: 0}: a}, nil
}

// tag names what session id's composer holds, the way the spec does. A
// tag with no content and no upload running for this session shows the
// attach error: the Thread that followed its upload said so, or a Thread
// mounted since found it with lostTags.
func (a *draftMoveAdapter) tag(id string) string {
	d := a.draft[id]
	at := a.atts[id]
	switch {
	case strings.TrimSpace(d) == "":
		return "none"
	case dmWrapped.MatchString(d), dmImageSent.MatchString(d):
		return "expanded" // a tag's content as raw text, no longer a chip
	case dmPasteTag.MatchString(d):
		if at != nil && len(at.pastes) > 0 {
			return "paste"
		}
		return "lost"
	case dmImageTag.MatchString(d):
		switch {
		case at != nil && len(at.images) > 0 && at.images[0] != "":
			return "image"
		case a.pending != nil && a.pending.owner == id:
			return "up_image"
		}
		return "lost"
	}
	return "text"
}

// dmSent classifies what Send delivered: what its tag became. A bare
// tag in it is a placeholder, whatever else it carries.
func dmSent(text string) string {
	switch {
	case text == "":
		return "none"
	case dmImageTag.MatchString(text), dmPasteTag.MatchString(text):
		return "placeholder"
	case dmImageSent.MatchString(text):
		return "image"
	case dmWrapped.MatchString(text):
		return "paste"
	}
	return "text"
}

// GetState is the Move role. moved is whether serve lists a project
// session this walk created; sent is the new session's recorded input.
func (a *draftMoveAdapter) GetState() (map[string]any, error) {
	moved, err := a.moved()
	if err != nil {
		return nil, err
	}
	sent := "none"
	if a.newID != "" {
		es, err := history.Read(a.historyPath(a.newID))
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		for _, e := range es {
			if e.Kind == "input" {
				sent = dmSent(history.Prompt(e))
			}
		}
	}
	newTag := "absent"
	if a.newID != "" {
		newTag = a.tag(a.newID)
	}
	return map[string]any{
		"old_tag":  a.tag(a.oldID),
		"new_tag":  newTag,
		"view":     a.view,
		"moved":    moved,
		"typed":    a.typed,
		"old_keys": a.draft[a.oldID] != "" || a.atts[a.oldID] != nil || a.ask[a.oldID],
		"sent":     sent,
	}, nil
}

func (a *draftMoveAdapter) moved() (bool, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return false, err
	}
	for _, r := range rows {
		if a.created(r) {
			return true, nil
		}
	}
	return false, nil
}

// created is a session a create made this walk: a project thread, which
// serve parents to the project's main (main itself may be new too, when
// the project had none).
func (a *draftMoveAdapter) created(r serve.Row) bool {
	return !a.before[r.ID] && r.SpawnedBy != "" && (r.Project == a.healthy || r.Project == a.broken)
}

func (a *draftMoveAdapter) historyPath(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

func (a *draftMoveAdapter) historyWritten(id string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(a.historyPath(id)); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("session %s wrote no history in %s", id, actionTimeout)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *draftMoveAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

// mount is a Thread mounting on id: its effects store the draft it read,
// and with an empty draft drop the draft-ask and draft-atts keys.
func (a *draftMoveAdapter) mount(id string) {
	if strings.TrimSpace(a.draft[id]) == "" {
		delete(a.draft, id)
		delete(a.ask, id)
		delete(a.atts, id)
		return
	}
	a.ask[id] = true
}

// put is Thread's insert into the old draft, with the effects that store
// it: bough:draft, bough:draft-ask (a draft remembers the question it
// answers, "" for none) and bough:draft-atts when it has a paste or slot.
func (a *draftMoveAdapter) put(s string, at *dmAtts) {
	a.draft[a.oldID] += s
	a.ask[a.oldID] = true
	if at != nil {
		a.atts[a.oldID] = at
	}
	a.typed = true
}

func (a *draftMoveAdapter) editable() bool {
	return a.view == "old" && a.newID == "" && a.tag(a.oldID) == "none"
}

func (a *draftMoveAdapter) Type() error {
	if a.on("Type", a.editable()) {
		a.put(a.name("note "), nil)
	}
	return nil
}

// PasteLong: over 12 lines, take() keeps the paste as a tag.
func (a *draftMoveAdapter) PasteLong() error {
	if !a.on("PasteLong", a.editable()) {
		return nil
	}
	lines := make([]string, 20)
	for i := range lines {
		lines[i] = fmt.Sprintf("%s line %d", a.name("paste"), i)
	}
	a.put(fmt.Sprintf("[Pasted text #1 +%d lines] ", len(lines)), &dmAtts{pastes: []string{strings.Join(lines, "\n")}})
	return nil
}

// PasteImage is attach() up to the await: the tag lands with an empty
// slot and the upload is registered under the old session.
func (a *draftMoveAdapter) PasteImage() error {
	if a.on("PasteImage", a.editable()) {
		a.put("[Image #1] ", &dmAtts{images: []string{""}})
		a.pending = &dmUpload{owner: a.oldID, slot: 0}
	}
	return nil
}

func (a *draftMoveAdapter) uploading() bool {
	return a.tag(a.oldID) == "up_image" || (a.newID != "" && a.tag(a.newID) == "up_image")
}

// UploadOk is the upload answering: follow() writes the path into the
// slot of its owner's stored draft-atts, when that slot is still empty.
func (a *draftMoveAdapter) UploadOk() error {
	if !a.on("UploadOk", a.uploading()) {
		return nil
	}
	path, err := a.upload(apPNG)
	if err != nil {
		return fmt.Errorf("upload: %w", err)
	}
	u := a.pending
	a.pending = nil
	if at := a.atts[u.owner]; at != nil && u.slot < len(at.images) && at.images[u.slot] == "" {
		at.images[u.slot] = path
	}
	return nil
}

// UploadFail is the upload refused (an empty image is a 400): the slot
// stays empty and the Thread following it says so.
func (a *draftMoveAdapter) UploadFail() error {
	if !a.on("UploadFail", a.uploading()) {
		return nil
	}
	path, err := a.upload(nil)
	if err == nil {
		return fmt.Errorf("an empty image was stored at %s; the server should refuse it", path)
	}
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) {
		return fmt.Errorf("upload: %w", err)
	}
	a.pending = nil
	return nil
}

// upload is api.attach: POST /api/attachments with the image's type.
func (a *draftMoveAdapter) upload(body []byte) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/attachments", bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "image/png")
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
		return "", fmt.Errorf("POST /api/attachments: no path in %s", raw)
	}
	return out.Path, nil
}

// dmExpand is Thread's expand over a stored draft: a tag whose content
// is there becomes that content, one whose content is not stays a tag.
func dmExpand(d string, at *dmAtts) string {
	if at == nil {
		at = &dmAtts{}
	}
	d = dmImageTag.ReplaceAllStringFunc(d, func(m string) string {
		var i int
		fmt.Sscanf(m, "[Image #%d]", &i)
		if i-1 < len(at.images) && at.images[i-1] != "" {
			return fmt.Sprintf("[Image #%d: %s]", i, at.images[i-1])
		}
		return m
	})
	return dmPasteTag.ReplaceAllStringFunc(d, func(m string) string {
		var i, n int
		fmt.Sscanf(m, "[Pasted text #%d +%d lines]", &i, &n)
		if i-1 < len(at.pastes) {
			return fmt.Sprintf("<pasted-text lines=\"%d\">\n%s\n</pasted-text>", n, at.pastes[i-1])
		}
		return m
	})
}

// dmLost is lostTags: the draft has a tag whose content is gone.
func dmLost(d string, at *dmAtts) bool {
	if at == nil {
		at = &dmAtts{}
	}
	if m := dmImageTag.FindStringSubmatch(d); m != nil {
		var i int
		fmt.Sscanf(m[1], "%d", &i)
		if i-1 >= len(at.images) || at.images[i-1] == "" {
			return true
		}
	}
	if m := dmPasteTag.FindStringSubmatch(d); m != nil {
		var i int
		fmt.Sscanf(m[1], "%d", &i)
		if i-1 >= len(at.pastes) {
			return true
		}
	}
	return false
}

// failedBuild is orb.tsx's failedBuild over GET /api/projects: the last
// build failed and no image stands in for it.
func (a *draftMoveAdapter) failedBuild(slug string) (bool, error) {
	var r struct {
		Projects []struct {
			Slug string            `json:"slug"`
			Orb  *serve.OrbSummary `json:"orb"`
		} `json:"projects"`
	}
	if err := a.api(http.MethodGet, "/api/projects", nil, &r); err != nil {
		return false, err
	}
	for _, p := range r.Projects {
		if p.Slug == slug {
			return p.Orb != nil && p.Orb.Build == "failed" && !p.Orb.Built, nil
		}
	}
	return false, fmt.Errorf("project %q is not listed", slug)
}

// pick is the composer's Select: App's onStartProject, where
// confirmFailedBuild asks first on a failed build. The draft is read
// only once the create answers.
func (a *draftMoveAdapter) pick(slug string) error {
	a.picked = slug
	failed, err := a.failedBuild(slug)
	if err != nil {
		return err
	}
	if failed {
		a.view = "confirm"
		return nil
	}
	a.create()
	return nil
}

// create is api.create in flight: the child parks at llm-control's start
// hold, so CreateOk and CreateFail decide how the POST answers.
func (a *draftMoveAdapter) create() {
	a.view = "creating"
	control.HoldStart(a.t, a.dir)
	ch := make(chan dmCreate, 1)
	a.post = ch
	body := map[string]any{"cwd": a.s.Home, "prompt": "", "mode": "project", "project": a.picked}
	go func() {
		var r struct {
			Session serve.Row `json:"session"`
		}
		err := a.api(http.MethodPost, "/api/sessions", body, &r)
		ch <- dmCreate{r.Session, err}
	}()
}

func (a *draftMoveAdapter) collect() dmCreate {
	defer func() { a.post = nil }()
	select {
	case r := <-a.post:
		return r
	case <-time.After(actionTimeout):
		return dmCreate{err: fmt.Errorf("the create did not answer in %s", actionTimeout)}
	}
}

func (a *draftMoveAdapter) PickProject() error {
	if !a.on("PickProject", a.view == "old" && a.newID == "") {
		return nil
	}
	return a.pick(a.healthy)
}

func (a *draftMoveAdapter) PickFailedProject() error {
	if !a.on("PickFailedProject", a.view == "old" && a.newID == "") {
		return nil
	}
	return a.pick(a.broken)
}

func (a *draftMoveAdapter) StartAnyway() error {
	if a.on("StartAnyway", a.view == "confirm") {
		a.create()
	}
	return nil
}

func (a *draftMoveAdapter) CancelConfirm() error {
	if a.on("CancelConfirm", a.view == "confirm") {
		a.view, a.picked = "old", ""
	}
	return nil
}

// CreateOk lets the child start; onStartProject then moves the draft
// and opens the new session in its project.
func (a *draftMoveAdapter) CreateOk() error {
	if !a.on("CreateOk", a.view == "creating") {
		return nil
	}
	control.ReleaseStart(a.t, a.dir)
	r := a.collect()
	if r.err != nil {
		a.view = "old"
		return fmt.Errorf("create: %w", r.err)
	}
	a.newID = r.row.ID
	a.ids = append(a.ids, r.row.ID)
	a.moveDraft()
	a.view = "new"
	a.mount(a.newID)
	return nil
}

// moveDraft is app.tsx's moveDraft after api.create answers: the stored
// draft and its draft-atts as they are now, the old keys dropped, and
// the upload in flight re-registered under the new session.
func (a *draftMoveAdapter) moveDraft() {
	if strings.TrimSpace(a.draft[a.oldID]) != "" {
		a.draft[a.newID] = a.draft[a.oldID]
		if at := a.atts[a.oldID]; at != nil {
			a.atts[a.newID] = at
		}
	}
	delete(a.draft, a.oldID)
	delete(a.atts, a.oldID)
	delete(a.ask, a.oldID)
	if a.pending != nil && a.pending.owner == a.oldID {
		a.pending.owner = a.newID
	}
	a.picked = ""
}

// CreateFail makes the held child exit: the POST answers an error, the
// page alerts, and nothing moves.
func (a *draftMoveAdapter) CreateFail() error {
	if !a.on("CreateFail", a.view == "creating") {
		return nil
	}
	if a.createFailStarts {
		control.ReleaseStart(a.t, a.dir)
	} else {
		control.ExitStart(a.t, a.dir)
	}
	r := a.collect()
	a.view, a.picked = "old", ""
	if r.err == nil {
		// The page would have moved the draft; the spec's CreateFail
		// does not. moved, read off the server, says so.
		a.ids = append(a.ids, r.row.ID)
	}
	return nil
}

func (a *draftMoveAdapter) GoOld() error {
	if a.on("GoOld", a.newID != "" && a.view == "new") {
		a.view = "old"
		a.mount(a.oldID)
	}
	return nil
}

func (a *draftMoveAdapter) GoNew() error {
	if a.on("GoNew", a.newID != "" && a.view == "old") {
		a.view = "new"
		a.mount(a.newID)
	}
	return nil
}

// Send is Thread's send in the new session: refused while an upload
// runs or with a lost tag, otherwise the draft goes out expanded and has
// to land in the new session's history.
func (a *draftMoveAdapter) Send() error {
	t := ""
	if a.newID != "" {
		t = a.tag(a.newID)
	}
	if !a.on("Send", a.view == "new" && t != "" && t != "none" && t != "up_image") {
		return nil
	}
	d := strings.TrimSpace(a.draft[a.newID])
	at := a.atts[a.newID]
	if dmLost(d, at) {
		return nil
	}
	full := dmExpand(d, at)
	delete(a.draft, a.newID)
	delete(a.atts, a.newID)
	delete(a.ask, a.newID)
	// The create answers ~20 ms before the thread writes its history,
	// and serve 404s a prompt to a session it has no history for; a
	// person is never that fast, the walk is.
	if err := a.historyWritten(a.newID); err != nil {
		return err
	}
	control.Queue(a.t, a.dir, a.name("dm"), control.Turn{Mode: "ok", Text: "noted"})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.newID, full); err != nil {
		return fmt.Errorf("send: %w", err)
	}
	_, err := waitRow(a.s, a.newID, "the sent message to land and its turn to end", func(serve.Row) bool {
		es, err := history.Read(a.historyPath(a.newID))
		if err != nil {
			return false
		}
		in := false
		for _, e := range es {
			in = in || (e.Kind == "input" && history.Prompt(e) == full)
			if in && e.Kind == "done" {
				return true
			}
		}
		return false
	})
	return err
}

var draftMoveActions = map[string]map[string]fmbt.ActionFunc{"Move": {
	"Type":              action((*draftMoveAdapter).Type),
	"PasteLong":         action((*draftMoveAdapter).PasteLong),
	"PasteImage":        action((*draftMoveAdapter).PasteImage),
	"UploadOk":          action((*draftMoveAdapter).UploadOk),
	"UploadFail":        action((*draftMoveAdapter).UploadFail),
	"PickProject":       action((*draftMoveAdapter).PickProject),
	"PickFailedProject": action((*draftMoveAdapter).PickFailedProject),
	"StartAnyway":       action((*draftMoveAdapter).StartAnyway),
	"CancelConfirm":     action((*draftMoveAdapter).CancelConfirm),
	"CreateOk":          action((*draftMoveAdapter).CreateOk),
	"CreateFail":        action((*draftMoveAdapter).CreateFail),
	"GoOld":             action((*draftMoveAdapter).GoOld),
	"GoNew":             action((*draftMoveAdapter).GoNew),
	"Send":              action((*draftMoveAdapter).Send),
}}

func draftMoveOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 8, "max-parallel-runs": 0}
}

// draftMoveHistory reads a created session's transcript as a path. Only
// what was sent is recorded, so the input is put back as the steps that
// make such a draft in the old session, the pick and the create, then
// the Send with its sent read off the recorded text. A session with no
// input is a create whose draft was never sent.
func draftMoveHistory(entries []history.Entry) []tracecheck.Step {
	f := func(k string) string { return "Move#0." + k }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{f("view"): "old", f("moved"): false, f("new_tag"): "absent", f("sent"): "none"}}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: f(action), State: state})
	}
	sent := ""
	for _, e := range entries {
		if e.Kind == "input" && sent == "" {
			sent = dmSent(history.Prompt(e))
		}
	}
	switch sent {
	case "text":
		add("Type", map[string]any{f("old_tag"): "text"})
	case "paste":
		add("PasteLong", map[string]any{f("old_tag"): "paste"})
	case "image", "placeholder":
		add("PasteImage", map[string]any{f("old_tag"): "up_image"})
		if sent == "image" {
			add("UploadOk", map[string]any{f("old_tag"): "image"})
		}
	}
	add("PickProject", map[string]any{f("view"): "creating"})
	add("CreateOk", map[string]any{f("view"): "new", f("moved"): true, f("old_keys"): false})
	if sent != "" {
		add("Send", map[string]any{f("sent"): sent})
	}
	return steps
}

func init() { historyProjections["draft_move_start_project"] = draftMoveHistory }

// walkDraftMovePaths walks every generated path (the cover's walks over
// the checked-in graph) and compares the adapter's state with the
// spec's after each step. Every path runs, so one run reports every
// divergence.
func walkDraftMovePaths(a *draftMoveAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("draft_move_start_project", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walkPath(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Move#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *draftMoveAdapter) walkPath(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else {
			f, ok := draftMoveActions["Move"][strings.TrimPrefix(s.Action, "Move#0.")]
			if !ok {
				return fmt.Errorf("step %d: no action %s", j, s.Action)
			}
			_, err = f(a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Move#0.")
			if ok && got[f] != v {
				return fmt.Errorf("step %d (%s): %s is %v, the spec says %v (state %v)", j, s.Action, f, got[f], v, got)
			}
		}
	}
	return nil
}

// TestDraftMoveStartProject lets fizzbee-mbt walk the spec at random
// (the exhaustive run only); TestDraftMoveStartProjectPaths is the cover.
func TestDraftMoveStartProject(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newDraftMoveAdapter(t)
	err := runMBT(t, "draft_move_start_project", a, draftMoveActions, draftMoveOptions())
	t.Logf("actions run past the gate: %v", a.ran)
	if err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkDraftMoveHistories(t, a)
}

func TestDraftMoveStartProjectPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newDraftMoveAdapter(t)
	if err := walkDraftMovePaths(a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	if len(a.ids) == 0 {
		t.Fatal("no path created a session")
	}
	checkDraftMoveHistories(t, a)
}

// Every session a walk created left a transcript that is a path too.
func checkDraftMoveHistories(t *testing.T, a *draftMoveAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "draft_move_start_project"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), draftMoveHistory)
	}
	t.Logf("trace-checked %d created sessions' histories", len(a.ids))
}

// The projection is only a check if a transcript the model forbids is
// refused: a moved image sent as its bare placeholder.
func TestDraftMoveStartProjectHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "draft_move_start_project"))
	if err != nil {
		t.Fatal(err)
	}
	meta := history.Entry{Kind: "meta", Data: map[string]any{"mode": "project"}}
	in := func(s string) history.Entry { return history.Entry{Kind: "input", Data: map[string]any{"text": s}} }
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"unsent": {meta},
		"text":   {meta, in("note 1"), done},
		"paste":  {meta, in("<pasted-text lines=\"2\">\na\nb\n</pasted-text>"), done},
		"image":  {meta, in("[Image #1: /h/.bough/attachments/x.png]"), done},
	} {
		if v := g.Check(draftMoveHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(draftMoveHistory([]history.Entry{meta, in("[Image #1]"), done})); v == nil {
		t.Error("a moved image sent as its bare placeholder passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}

// A run whose CreateFail lets the child start must fail, or a green
// TestDraftMoveStartProjectPaths proves nothing. Every link: the wrong
// release shows on the CreateFail transition, which a walk that only
// reaches every state need not take.
func TestDraftMoveStartProjectCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newDraftMoveAdapter(t)
	a.createFailStarts = true
	err := walkDraftMovePaths(a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a run whose CreateFail lets the child start passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
