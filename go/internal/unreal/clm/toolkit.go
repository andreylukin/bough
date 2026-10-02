//go:build !windows

package clm

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/andreylukin/bough/internal/contextkit"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

const snapshotRetention = 8
const readPageLimit = 16 << 10

// Leave room for metadata under the default 40,000-rune native output cap,
// even when JSON escapes control characters or HTML-sensitive bytes sixfold.
const readEncodedTextLimit = 32 << 10
const opaqueTokenLimit = 256
const searchPageLimit = 100
const archiveLimit = 128

// contextMutex makes queued toolkit/native writes cancellable. Legacy methods
// retain Lock/Unlock; unrelated contexts never share this gate.
type contextMutex struct {
	once sync.Once
	gate chan struct{}
}

func (m *contextMutex) init() {
	m.once.Do(func() { m.gate = make(chan struct{}, 1); m.gate <- struct{}{} })
}
func (m *contextMutex) Lock()   { _ = m.lock(context.Background()) }
func (m *contextMutex) Unlock() { m.gate <- struct{}{} }
func (m *contextMutex) lock(ctx context.Context) error {
	m.init()
	if err := ctx.Err(); err != nil {
		return err
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-m.gate:
	}
	if err := ctx.Err(); err != nil {
		m.Unlock()
		return err
	}
	return nil
}

type contextSnapshot struct {
	info contextkit.Info
	text string
}

var _ contextkit.Capability = (*Context)(nil)

func hashText(b []byte) string   { sum := sha256.Sum256(b); return hex.EncodeToString(sum[:]) }
func (c *Context) owner() string { return hashText([]byte(c.statePath))[:24] }
func (c *Context) revision(b []byte) string {
	return fmt.Sprintf("%s:%d:%s", c.owner(), c.state.Generation, hashText(b))
}
func (c *Context) pending() error {
	if _, err := os.Lstat(c.statePath + ".pending"); err == nil {
		return contextkit.ErrRecoveryRequired
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}
func (c *Context) info(b []byte) contextkit.Info {
	lines := bytes.Count(b, []byte{'\n'})
	if len(b) > 0 && b[len(b)-1] != '\n' {
		lines++
	}
	return contextkit.Info{Revision: c.revision(b), ContentHash: hashText(b), Bytes: len(b), Lines: lines, Limit: c.limit}
}

func (c *Context) Inspect(ctx context.Context) (contextkit.Info, error) {
	if err := c.mu.lock(ctx); err != nil {
		return contextkit.Info{}, err
	}
	defer c.mu.Unlock()
	if err := c.pending(); err != nil {
		return contextkit.Info{}, err
	}
	b, err := c.read()
	if err != nil {
		return contextkit.Info{}, err
	}
	i := c.info(b)
	i.SnapshotID = c.owner() + ":" + rand.Text()
	c.snapshotMu.Lock()
	defer c.snapshotMu.Unlock()
	if c.snapshots == nil {
		c.snapshots = make(map[string]contextSnapshot)
	}
	c.snapshots[i.SnapshotID] = contextSnapshot{info: i, text: string(b)}
	c.snapshotOrder = append(c.snapshotOrder, i.SnapshotID)
	if len(c.snapshotOrder) > snapshotRetention {
		delete(c.snapshots, c.snapshotOrder[0])
		c.snapshotOrder = c.snapshotOrder[1:]
	}
	return i, nil
}
func (c *Context) snapshot(ctx context.Context, id string) (contextSnapshot, error) {
	if err := ctx.Err(); err != nil {
		return contextSnapshot{}, err
	}
	if len(id) > opaqueTokenLimit {
		return contextSnapshot{}, contextkit.ErrForeignReference
	}
	c.snapshotMu.RLock()
	defer c.snapshotMu.RUnlock()
	// These bytes were validated when captured. Reading them neither waits
	// for a live-file commit nor tries to repair a newer pending transaction.
	if !strings.HasPrefix(id, c.owner()+":") {
		return contextSnapshot{}, contextkit.ErrForeignReference
	}
	s, ok := c.snapshots[id]
	if !ok {
		return contextSnapshot{}, contextkit.ErrSnapshotExpired
	}
	return s, ctx.Err()
}

func boundary(s string, n int) bool {
	return n >= 0 && n <= len(s) && (n == len(s) || utf8.RuneStart(s[n]))
}
func validRange(s string, start, end int) bool {
	return start <= end && boundary(s, start) && boundary(s, end)
}
func (c *Context) Read(ctx context.Context, q contextkit.ReadRequest) (contextkit.ReadResult, error) {
	s, err := c.snapshot(ctx, q.SnapshotID)
	if err != nil {
		return contextkit.ReadResult{}, err
	}
	if !boundary(s.text, q.Offset) || q.Limit < 0 {
		return contextkit.ReadResult{}, fmt.Errorf("context read: invalid UTF-8 byte offset or limit")
	}
	n := q.Limit
	if n == 0 || n > readPageLimit {
		n = readPageLimit
	}
	end := min(len(s.text), q.Offset+n)
	for end > q.Offset && !boundary(s.text, end) {
		end--
	}
	if end == q.Offset && end < len(s.text) {
		return contextkit.ReadResult{}, fmt.Errorf("context read: limit does not fit the next UTF-8 character; use at least 4 bytes")
	}
	end = q.Offset + encodedReadPrefix(s.text[q.Offset:end])
	return contextkit.ReadResult{Info: s.info, Text: s.text[q.Offset:end], Offset: q.Offset, NextOffset: end, HasMore: end < len(s.text)}, ctx.Err()
}

// Use the same JSON encoder as the tool row rather than assuming an escape
// policy. The predicate floors byte offsets to UTF-8 boundaries; binary search
// finds the longest prefix that fits without dropping or repeating any bytes.
func encodedReadPrefix(text string) int {
	encoded, _ := json.Marshal(text)
	if len(encoded) <= readEncodedTextLimit {
		return len(text)
	}
	lo, hi := 0, len(text)
	for lo < hi {
		mid := lo + (hi-lo+1)/2
		end := mid
		for end > 0 && !boundary(text, end) {
			end--
		}
		encoded, _ = json.Marshal(text[:end])
		if len(encoded) <= readEncodedTextLimit {
			lo = mid
		} else {
			hi = mid - 1
		}
	}
	for lo > 0 && !boundary(text, lo) {
		lo--
	}
	return lo
}

func (c *Context) Search(ctx context.Context, q contextkit.SearchRequest) (contextkit.SearchResult, error) {
	s, err := c.snapshot(ctx, q.SnapshotID)
	if err != nil {
		return contextkit.SearchResult{}, err
	}
	if q.Query == "" || !utf8.ValidString(q.Query) || len(q.Query) > readPageLimit || !boundary(s.text, q.Offset) || q.Limit < 0 {
		return contextkit.SearchResult{}, fmt.Errorf("context search: provide a nonempty UTF-8 literal query and valid offset/limit")
	}
	n := q.Limit
	if n == 0 || n > searchPageLimit {
		n = searchPageLimit
	}
	r := contextkit.SearchResult{Info: s.info, Matches: []contextkit.Match{}, NextOffset: q.Offset}
	for at := q.Offset; at < len(s.text); {
		if err := ctx.Err(); err != nil {
			return contextkit.SearchResult{}, err
		}
		rel := strings.Index(s.text[at:], q.Query)
		if rel < 0 {
			r.NextOffset = len(s.text)
			break
		}
		start := at + rel
		end := start + len(q.Query)
		if len(r.Matches) == n {
			r.HasMore = true
			break
		}
		r.Matches = append(r.Matches, contextkit.Match{Start: start, End: end, Line: strings.Count(s.text[:start], "\n") + 1})
		r.NextOffset = end
		at = end
	}
	return r, nil
}

func (c *Context) base(ctx context.Context, expected string) (string, error) {
	if len(expected) > opaqueTokenLimit {
		return "", fmt.Errorf("context revision token exceeds %d bytes", opaqueTokenLimit)
	}
	if err := c.mu.lock(ctx); err != nil {
		return "", err
	}
	defer c.mu.Unlock()
	if err := c.pending(); err != nil {
		return "", err
	}
	b, err := c.read()
	if err != nil {
		return "", err
	}
	if expected == "" || expected != c.revision(b) {
		return "", &contextkit.Conflict{Expected: expected, Current: c.revision(b)}
	}
	return string(b), nil
}
func (c *Context) candidate(before string, edits []contextkit.Edit) (string, error) {
	if len(edits) == 0 || len(edits) > 64 {
		return "", fmt.Errorf("context edit: supply 1..64 ordered edits")
	}
	after := before
	for i, e := range edits {
		if !validRange(after, e.Start, e.End) || !utf8.ValidString(e.Text) {
			return "", fmt.Errorf("context edit %d: invalid UTF-8 range or text", i)
		}
		if len(after)-(e.End-e.Start)+len(e.Text) > c.limit {
			return "", fmt.Errorf("context edit %d exceeds %d-byte cap", i, c.limit)
		}
		after = after[:e.Start] + e.Text + after[e.End:]
	}
	return after, nil
}
func (c *Context) commit(ctx context.Context, expected, before, after string, dry bool, archiveID, excerpt string) (contextkit.EditResult, error) {
	if err := c.mu.lock(ctx); err != nil {
		return contextkit.EditResult{}, err
	}
	defer c.mu.Unlock()
	if err := c.pending(); err != nil {
		return contextkit.EditResult{}, err
	}
	b, err := c.read()
	if err != nil {
		return contextkit.EditResult{}, err
	}
	if expected != c.revision(b) || before != string(b) {
		return contextkit.EditResult{}, &contextkit.Conflict{Expected: expected, Current: c.revision(b)}
	}
	if archiveID != "" {
		if len(c.state.Archives) >= archiveLimit {
			if _, ok := c.state.Archives[archiveID]; !ok {
				return contextkit.EditResult{}, fmt.Errorf("context archive limit reached")
			}
		}
		if _, err := c.existingArchive(archiveID, excerpt); err != nil {
			return contextkit.EditResult{}, err
		}
	}
	r := contextkit.EditResult{Revision: expected, ContentHash: hashText([]byte(after)), Bytes: len(after), DryRun: dry, Changed: before != after}
	if dry {
		return r, nil
	}
	if err := contextkit.CheckCommit(ctx); err != nil {
		return contextkit.EditResult{}, err
	}
	next := c.state
	next.Generation++
	if archiveID != "" {
		if err := c.saveArchive(archiveID, excerpt); err != nil {
			return contextkit.EditResult{}, err
		}
		next.Archives = cloneStrings(next.Archives)
		next.Archives[archiveID] = hashText([]byte(excerpt))
	}
	// Once journal commit begins it is completed or left recoverable; do not
	// report cancellation after durable mutation and imply nothing happened.
	if err := c.saveTransaction(string(b), after, next); err != nil {
		return contextkit.EditResult{}, err
	}
	r.Revision = c.revision([]byte(after))
	return r, nil
}
func cloneStrings(m map[string]string) map[string]string {
	n := make(map[string]string, len(m))
	for k, v := range m {
		n[k] = v
	}
	return n
}
func (c *Context) saveTransaction(before, after string, next diskState) error {
	next.Version = stateVersion
	tx := transaction{Before: before, After: after, Next: next}
	b, _ := json.Marshal(tx)
	if err := atomicWrite(c.statePath+".pending", b); err != nil {
		return err
	}
	if err := atomicWrite(c.path, []byte(after)); err != nil {
		return err
	}
	b, _ = json.Marshal(next)
	if err := atomicWrite(c.statePath, b); err != nil {
		return err
	}
	if err := os.Remove(c.statePath + ".pending"); err != nil {
		return err
	}
	c.state = next
	return nil
}
func (c *Context) Edit(ctx context.Context, q contextkit.EditRequest) (contextkit.EditResult, error) {
	before, err := c.base(ctx, q.ExpectedRevision)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	after, err := c.candidate(before, q.Edits)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	return c.commit(ctx, q.ExpectedRevision, before, after, q.DryRun, "", "")
}
func (c *Context) Offload(ctx context.Context, q contextkit.OffloadRequest) (contextkit.OffloadResult, error) {
	before, err := c.base(ctx, q.ExpectedRevision)
	if err != nil {
		return contextkit.OffloadResult{}, err
	}
	if q.Start == q.End {
		return contextkit.OffloadResult{}, fmt.Errorf("context offload: excerpt must not be empty")
	}
	if !validRange(before, q.Start, q.End) {
		return contextkit.OffloadResult{}, fmt.Errorf("context offload: invalid UTF-8 excerpt range")
	}
	excerpt := before[q.Start:q.End]
	id := c.owner() + "-" + hashText([]byte(excerpt))
	replacement := "[[clm-archive:" + id + "]]"
	if q.Replacement != nil {
		replacement = *q.Replacement
	}
	after, err := c.candidate(before, []contextkit.Edit{{Start: q.Start, End: q.End, Text: replacement}})
	if err != nil {
		return contextkit.OffloadResult{}, err
	}
	r, err := c.commit(ctx, q.ExpectedRevision, before, after, q.DryRun, id, excerpt)
	if err != nil {
		return contextkit.OffloadResult{}, err
	}
	if q.DryRun {
		id = ""
	}
	return contextkit.OffloadResult{EditResult: r, ArchiveID: id, ArchivedBytes: len(excerpt)}, err
}
func validArchiveID(id string) bool {
	if len(id) != 89 || id[24] != '-' {
		return false
	}
	_, a := hex.DecodeString(id[:24])
	_, b := hex.DecodeString(id[25:])
	return a == nil && b == nil
}
func (c *Context) archivePath(id string) string { return filepath.Join(c.statePath+".archives", id) }
func readArchive(path, want string, limit int) (string, error) {
	dir, err := os.Lstat(filepath.Dir(path))
	if err != nil {
		return "", err
	}
	if !dir.IsDir() || dir.Mode()&os.ModeSymlink != 0 {
		return "", fmt.Errorf("context archive directory must not be a symlink")
	}
	st, err := os.Lstat(path)
	if err != nil {
		return "", fmt.Errorf("context archive unavailable: %w", err)
	}
	if !st.Mode().IsRegular() || st.Size() > int64(limit) {
		return "", fmt.Errorf("context archive is not a bounded regular file")
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	if len(b) > limit || !utf8.Valid(b) || hashText(b) != want {
		return "", fmt.Errorf("context archive checksum/UTF-8 validation failed")
	}
	return string(b), nil
}
func (c *Context) existingArchive(id, text string) (bool, error) {
	path := c.archivePath(id)
	if st, err := os.Lstat(filepath.Dir(path)); err == nil {
		if !st.IsDir() || st.Mode()&os.ModeSymlink != 0 {
			return false, fmt.Errorf("context archive directory must not be a symlink")
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return false, err
	}
	if _, err := os.Lstat(path); err == nil {
		got, err := readArchive(path, hashText([]byte(text)), c.limit)
		if err != nil {
			return false, err
		}
		if got != text {
			return false, fmt.Errorf("context archive collision")
		}
		return true, nil
	} else if !errors.Is(err, os.ErrNotExist) {
		return false, err
	}
	return false, nil
}
func (c *Context) saveArchive(id, text string) error {
	exists, err := c.existingArchive(id, text)
	if err != nil || exists {
		return err
	}
	return atomicWrite(c.archivePath(id), []byte(text))
}

func (c *Context) Restore(ctx context.Context, q contextkit.RestoreRequest) (contextkit.EditResult, error) {
	if !validArchiveID(q.ArchiveID) {
		return contextkit.EditResult{}, contextkit.ErrForeignReference
	}
	before, err := c.base(ctx, q.ExpectedRevision)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	if err := c.mu.lock(ctx); err != nil {
		return contextkit.EditResult{}, err
	}
	want, ok := c.state.Archives[q.ArchiveID]
	c.mu.Unlock()
	if !ok {
		return contextkit.EditResult{}, contextkit.ErrForeignReference
	}
	excerpt, err := readArchive(c.archivePath(q.ArchiveID), want, c.limit)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	end := q.End
	if end == 0 {
		end = len(excerpt)
	}
	if !validRange(excerpt, q.Start, end) {
		return contextkit.EditResult{}, fmt.Errorf("context restore: invalid excerpt range")
	}
	after, err := c.candidate(before, []contextkit.Edit{{Start: q.Offset, End: q.Offset, Text: excerpt[q.Start:end]}})
	if err != nil {
		return contextkit.EditResult{}, err
	}
	return c.commit(ctx, q.ExpectedRevision, before, after, q.DryRun, "", "")
}

func canonicalPath(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	parent, err := filepath.EvalSymlinks(filepath.Dir(abs))
	if err != nil {
		return "", err
	}
	return filepath.Join(parent, filepath.Base(abs)), nil
}

// MutateFile coordinates a normal native file tool after its permission and
// hook processing. It never invokes mutate for an unrelated path. The callback
// must not call Context methods: the context-specific gate is held throughout.
func (c *Context) MutateFile(ctx context.Context, path string, mutate func() error) (bool, error) {
	resolved, err := canonicalPath(path)
	if err != nil {
		return false, nil
	}
	if resolved != c.managedPath {
		if target, e := filepath.EvalSymlinks(resolved); e == nil && target == c.managedPath {
			return true, fmt.Errorf("engine-clm: context must not be written through a symlink")
		}
		return false, nil
	}
	if err := c.mu.lock(ctx); err != nil {
		return true, err
	}
	defer c.mu.Unlock()
	if err := c.pending(); err != nil {
		return true, err
	}
	if st, err := os.Lstat(path); err == nil && (!st.Mode().IsRegular() || st.Mode()&os.ModeSymlink != 0) {
		return true, fmt.Errorf("engine-clm: context must be a regular file, not a symlink")
	}
	if err := contextkit.CheckCommit(ctx); err != nil {
		return true, err
	}
	// Persist the generation first: even a failed callback may partially write.
	// This preserves the private event cursor and detects cooperative ABA edits.
	next := c.state
	next.Version = stateVersion
	next.Generation++
	b, _ := json.Marshal(next)
	if err := atomicWrite(c.statePath, b); err != nil {
		return true, err
	}
	c.state = next
	return true, mutate()
}

func isContextTool(name string) bool {
	switch name {
	case "context_inspect", "context_read", "context_search", "context_edit", "context_offload", "context_restore":
		return true
	}
	return false
}
func renderContextItem(it ullm.Item, calls map[string]string) string {
	switch d := it.Data.(type) {
	case ullm.ToolCall:
		if isContextTool(d.Name) {
			return fmt.Sprintf("\n\n[context tool call %s %s]\n(arguments retained in audit)", receiptID(d.CallID), d.Name)
		}
	case ullm.ToolResult:
		if name, ok := calls[d.CallID]; ok {
			return fmt.Sprintf("\n\n[context tool result %s %s]\n(result retained in audit; inspect/read current context when needed)", receiptID(d.CallID), name)
		}
	}
	return render(it)
}

func receiptID(id string) string {
	if len(id) <= 128 {
		return id
	}
	end := 128
	for end > 0 && !utf8.RuneStart(id[end]) {
		end--
	}
	return id[:end] + "…"
}

func isContextItem(it ullm.Item, calls map[string]string) bool {
	switch d := it.Data.(type) {
	case ullm.ToolCall:
		return isContextTool(d.Name)
	case ullm.ToolResult:
		_, ok := calls[d.CallID]
		return ok
	}
	return false
}
