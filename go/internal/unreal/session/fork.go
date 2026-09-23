package session

import (
	"context"
	"slices"

	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
)

// The harness fork copies the parent's items but strips the operations
// off every inherited call status (localfile inheritItem: "without making
// inherited operations dispatchable"), and a coordinator rebuilding its
// context adds no result for a call whose operations it cannot find. A
// forked session's model then saw every earlier call without a result:
// the Messages adapter filled in "No result was recorded" with is_error,
// so the model believed each one failed, and the Responses API got a
// function_call with no output at all.
//
// forkedStore puts the operations back as the coordinator reads its
// history. They come from the parent's own file, where the item with the
// same sequence still has them (a fork keeps sequences), and from its
// parent in turn for a fork of a fork. The coordinator's replay then
// renders each inherited result exactly as the parent did, in the
// parent's order, and the fork item that follows clears them from its
// state, so nothing inherited is dispatched again.
type forkedStore struct {
	sessionstore.Store
	id  session.ID
	ops map[sessionstore.Sequence][]operation.Operation
}

func (s forkedStore) Items(ctx context.Context, id session.ID, after sessionstore.Sequence, limit int) (sessionstore.Page, error) {
	page, err := s.Store.Items(ctx, id, after, limit)
	if err != nil || id != s.id {
		return page, err
	}
	page.Items = slices.Clone(page.Items)
	for i, it := range page.Items {
		st, ok := it.Data.(sessionstore.ToolCallStatus)
		if ops := s.ops[it.Sequence]; ok && len(st.Operations) == 0 && len(ops) > 0 {
			st.Operations = ops
			page.Items[i].Data = st
		}
	}
	return page, nil
}

// sessions is the store a coordinator reads id's history through: the
// store itself, or forkedStore when id was forked and inherited calls.
func (r *Runtime) sessions(ctx context.Context, id session.ID) sessionstore.Store {
	items, err := r.allItems(ctx, id)
	if err != nil {
		return r.store
	}
	var parent session.ID
	need := map[sessionstore.Sequence]bool{}
	for _, it := range items {
		switch d := it.Data.(type) {
		case sessionstore.Fork:
			parent = d.ParentID // the last fork item is this session's own
		case sessionstore.ToolCallStatus:
			if len(d.Operations) == 0 && len(d.Status.WaitingFor) > 0 {
				need[it.Sequence] = true
			}
		}
	}
	ops := map[sessionstore.Sequence][]operation.Operation{}
	for parent != "" && len(need) > 0 {
		pitems, err := r.allItems(ctx, parent)
		if err != nil {
			break // the parent's file is gone: nothing left to recover
		}
		next := session.ID("")
		for _, it := range pitems {
			switch d := it.Data.(type) {
			case sessionstore.Fork:
				next = d.ParentID
			case sessionstore.ToolCallStatus:
				if need[it.Sequence] && len(d.Operations) > 0 {
					ops[it.Sequence] = d.Operations
					delete(need, it.Sequence)
				}
			}
		}
		parent = next
	}
	if len(ops) == 0 {
		return r.store
	}
	return forkedStore{Store: r.store, id: id, ops: ops}
}

func (r *Runtime) allItems(ctx context.Context, id session.ID) ([]sessionstore.Item, error) {
	var out []sessionstore.Item
	cur := sessionstore.BeforeFirst
	for {
		page, err := r.store.Items(ctx, id, cur, 256)
		if err != nil {
			return nil, err
		}
		out = append(out, page.Items...)
		if !page.More || page.NextAfter <= cur {
			return out, nil
		}
		cur = page.NextAfter
	}
}
