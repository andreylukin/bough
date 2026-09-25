package serve

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// A request id is the page's name for one send, kept across its retries
// (tests/model/specs/prompt_idempotency_reload.fizz, R1 and R2). Without
// it a Retry after a lost answer could not tell "serve never had it" from
// "the answer got lost", and wrote the line a second time: the same
// prompt recorded twice, or run as a steer of its own first copy.
//
// serve writes a line for an id at most once while that line can still
// land: while the child it went to is alive and has not recorded it, and
// for good once it has. A line whose child died unread (a restart, a
// crash) is gone, so the same id writes it again. The record survives a
// serve restart in ~/.bough/serve/requests/<session>.jsonl, because the
// question after a restart is exactly the one it answers.

// Request states, as GET /api/sessions/{id}/prompts/{rid} reports them.
const (
	ReqNone   = "none"   // never had it, or its child died before reading it
	ReqUnread = "unread" // written to the live child, not recorded yet
	ReqLanded = "landed" // recorded as an input
)

// promptReq is one id's line: its text and the newest history seq when
// serve wrote it, so an older input with the same text cannot land it.
type promptReq struct {
	Rid   string `json:"rid"`
	Text  string `json:"text"`
	After int64  `json:"after"`
	ch    *child // the process it went to; nil for a record read off disk
}

// SendOnce is Send under a request id: nothing is written when the id's
// line has landed or is still unread by the live child.
func (s *Supervisor) SendOnce(id, rid, text string) error {
	l := s.reqLock(id)
	l.Lock()
	defer l.Unlock()
	if s.reqState(id, rid) != ReqNone {
		return nil
	}
	var after int64
	if es, err := s.Entries(id); err == nil && len(es) > 0 {
		after = es[len(es)-1].Seq
	}
	if err := s.Send(id, text); err != nil {
		return err
	}
	r := &promptReq{Rid: rid, Text: strings.TrimSpace(text), After: after}
	s.mu.Lock()
	r.ch = s.kids[id]
	if s.reqs == nil {
		s.reqs = map[string]map[string]*promptReq{}
	}
	if s.reqs[id] == nil {
		s.reqs[id] = map[string]*promptReq{}
	}
	s.reqs[id][rid] = r
	s.mu.Unlock()
	// The line is written: a record that failed to save costs only the
	// dedup after a restart, never the send.
	if err := s.saveReq(id, r); err != nil {
		fmt.Fprintf(os.Stderr, "bough: serve: %v\n", err)
	}
	return nil
}

// PromptState is what serve knows about a request id (R2).
func (s *Supervisor) PromptState(id, rid string) string {
	l := s.reqLock(id)
	l.Lock()
	defer l.Unlock()
	return s.reqState(id, rid)
}

func (s *Supervisor) reqLock(id string) *sync.Mutex {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reqLocks == nil {
		s.reqLocks = map[string]*sync.Mutex{}
	}
	l := s.reqLocks[id]
	if l == nil {
		l = &sync.Mutex{}
		s.reqLocks[id] = l
	}
	return l
}

// reqState reads the id's record (memory, else disk) against history
// and the live child. Caller holds the session's reqLock.
func (s *Supervisor) reqState(id, rid string) string {
	s.mu.Lock()
	r := s.reqs[id][rid]
	s.mu.Unlock()
	if r == nil {
		r = s.loadReq(id, rid)
	}
	if r == nil {
		return ReqNone
	}
	if es, err := s.Entries(id); err == nil {
		for _, e := range es {
			if e.Kind == "input" && e.Seq > r.After && strings.TrimSpace(inputText(e.Data)) == r.Text {
				return ReqLanded
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if r.ch != nil && !r.ch.dropped && s.kids[id] == r.ch {
		return ReqUnread
	}
	return ReqNone
}

// inputText is what was typed: an input entry's text also carries @file
// expansions and injected skills, and keeps the typed line apart then.
func inputText(d map[string]any) string {
	if t, ok := d["typed"].(string); ok {
		return t
	}
	t, _ := d["text"].(string)
	return t
}

func (s *Supervisor) reqPath(id string) string {
	if s.opt.MetaPath == "" {
		return ""
	}
	return filepath.Join(filepath.Dir(s.opt.MetaPath), "requests", id+".jsonl")
}

func (s *Supervisor) saveReq(id string, r *promptReq) error {
	p := s.reqPath(id)
	if p == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return fmt.Errorf("serve: supervisor: request record: %w", err)
	}
	f, err := os.OpenFile(p, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return fmt.Errorf("serve: supervisor: request record: %w", err)
	}
	defer f.Close()
	b, _ := json.Marshal(r)
	if _, err := f.Write(append(b, '\n')); err != nil {
		return fmt.Errorf("serve: supervisor: request record %s: %w", p, err)
	}
	return nil
}

// loadReq is the id's newest record on disk: a line rewritten after its
// child died is recorded again with a later After.
func (s *Supervisor) loadReq(id, rid string) *promptReq {
	p := s.reqPath(id)
	if p == "" {
		return nil
	}
	f, err := os.Open(p)
	if err != nil {
		if !errors.Is(err, os.ErrNotExist) {
			fmt.Fprintf(os.Stderr, "bough: serve: request record: %v\n", err)
		}
		return nil
	}
	defer f.Close()
	var out *promptReq
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16*1024*1024)
	for sc.Scan() {
		var r promptReq
		if json.Unmarshal(sc.Bytes(), &r) == nil && r.Rid == rid {
			out = &r
		}
	}
	return out
}

// CreateOnce is Create under a request id: a create whose answer was
// lost and is retried returns the session the first one made instead of
// making a second. The id is kept in the session's meta, so a restart
// between the two does not forget it.
func (s *Supervisor) CreateOnce(opt CreateOptions, rid string) (id string, made bool, err error) {
	s.createReqMu.Lock()
	defer s.createReqMu.Unlock()
	s.mu.Lock()
	for sid, m := range s.meta {
		if m.Request == rid {
			s.mu.Unlock()
			return sid, false, nil
		}
	}
	s.mu.Unlock()
	id, err = s.Create(opt)
	if err != nil {
		return id, id != "", err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.meta[id]
	m.Request = rid
	s.meta[id] = m
	if err := s.saveMetaLocked(); err != nil {
		fmt.Fprintf(os.Stderr, "bough: serve: %v\n", err)
	}
	return id, true, nil
}
