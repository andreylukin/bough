// Package contextkit is the harness-independent contract for editable context.
package contextkit

import (
	"context"
	"errors"
	"fmt"
)

var (
	ErrConflict         = errors.New("context revision conflict")
	ErrSnapshotExpired  = errors.New("context snapshot expired; inspect again")
	ErrForeignReference = errors.New("reference does not belong to this context")
	ErrRecoveryRequired = errors.New("context transaction requires recovery before this operation")
)

// Capability is scoped to one main session or one individual child session.
// Offsets are UTF-8 byte offsets; edit ranges are half-open and each ordered
// edit addresses the result of the preceding edit. Revision is an opaque CAS
// token, distinct from ContentHash (which identifies only the live bytes).
type Capability interface {
	Inspect(context.Context) (Info, error)
	Read(context.Context, ReadRequest) (ReadResult, error)
	Search(context.Context, SearchRequest) (SearchResult, error)
	Edit(context.Context, EditRequest) (EditResult, error)
	Offload(context.Context, OffloadRequest) (OffloadResult, error)
	Restore(context.Context, RestoreRequest) (EditResult, error)
}

type Info struct {
	SnapshotID  string `json:"snapshot_id"`
	Revision    string `json:"revision"`
	ContentHash string `json:"content_hash"`
	Bytes       int    `json:"bytes"`
	Lines       int    `json:"lines"`
	Limit       int    `json:"limit"`
}
type ReadRequest struct {
	SnapshotID string `json:"snapshot_id"`
	Offset     int    `json:"offset,omitempty"`
	Limit      int    `json:"limit,omitempty"`
}
type ReadResult struct {
	Info
	Text       string `json:"text"`
	Offset     int    `json:"offset"`
	NextOffset int    `json:"next_offset"`
	HasMore    bool   `json:"has_more"`
}
type SearchRequest struct {
	SnapshotID string `json:"snapshot_id"`
	Query      string `json:"query"`
	Offset     int    `json:"offset,omitempty"`
	Limit      int    `json:"limit,omitempty"`
}
type Match struct {
	Start int `json:"start"`
	End   int `json:"end"`
	Line  int `json:"line"`
}
type SearchResult struct {
	Info
	Matches    []Match `json:"matches"`
	NextOffset int     `json:"next_offset"`
	HasMore    bool    `json:"has_more"`
}
type Edit struct {
	Start int    `json:"start"`
	End   int    `json:"end"`
	Text  string `json:"text"`
}
type EditRequest struct {
	ExpectedRevision string `json:"expected_revision"`
	Edits            []Edit `json:"edits"`
	DryRun           bool   `json:"dry_run,omitempty"`
}
type EditResult struct {
	Revision    string `json:"revision"`
	ContentHash string `json:"content_hash"`
	Bytes       int    `json:"bytes"`
	DryRun      bool   `json:"dry_run"`
	Changed     bool   `json:"changed"`
}
type OffloadRequest struct {
	ExpectedRevision string  `json:"expected_revision"`
	Start            int     `json:"start"`
	End              int     `json:"end"`
	Replacement      *string `json:"replacement,omitempty"`
	DryRun           bool    `json:"dry_run,omitempty"`
}
type OffloadResult struct {
	EditResult
	ArchiveID     string `json:"archive_id,omitempty"`
	ArchivedBytes int    `json:"archived_bytes"`
}
type RestoreRequest struct {
	ExpectedRevision string `json:"expected_revision"`
	ArchiveID        string `json:"archive_id"`
	Offset           int    `json:"offset"`
	// Start/End optionally select a byte range of the archived excerpt.
	// End zero means the complete excerpt from Start onward.
	Start  int  `json:"start,omitempty"`
	End    int  `json:"end,omitempty"`
	DryRun bool `json:"dry_run,omitempty"`
}

// Conflict reports the current CAS token without requiring an extra inspect.
type Conflict struct {
	Expected string `json:"expected_revision"`
	Current  string `json:"current_revision"`
}

func (e *Conflict) Error() string {
	return fmt.Sprintf("%s: expected %s; current %s", ErrConflict, e.Expected, e.Current)
}
func (e *Conflict) Unwrap() error { return ErrConflict }

// WithCommitCheck attaches a bounded, nonblocking policy revalidation. Callers
// perform permission prompts and runtime readiness waits before entering a
// mutation; this callback only validates that their approved policy is current.
func WithCommitCheck(ctx context.Context, check func() error) context.Context {
	if previous, ok := ctx.Value(commitCheckKey{}).(func() error); ok && previous != nil {
		next := check
		check = func() error {
			if err := previous(); err != nil {
				return err
			}
			if next != nil {
				return next()
			}
			return nil
		}
	}
	return context.WithValue(ctx, commitCheckKey{}, check)
}

type commitCheckKey struct{}

func CheckCommit(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if check, ok := ctx.Value(commitCheckKey{}).(func() error); ok && check != nil {
		return check()
	}
	return nil
}
