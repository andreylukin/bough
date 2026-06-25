//// Node-wide serialization for session writes (Phase 2 §3). Each live session
//// is one JSONL file rewritten whole on save, so two concurrent runs doing
//// load → mutate → save would lose-update each other. `mutate` funnels every
//// such write through a per-session `global:trans` lock: concurrent runs on
//// different branches still run in parallel, but their tree writes serialize.

import bough_core/session.{type SessionTree}
import bough_server/session_manager.{type StoreError}

@external(erlang, "bough_ffi", "with_session_lock")
fn with_lock(key: String, f: fn() -> a) -> a

/// Serialized read-modify-write: load the freshest tree, apply `f`, save —
/// all under the session's lock. Loading *inside* the lock is the point: a run
/// computes its turn from a stale snapshot, but appends it onto whatever the
/// tree looks like now, so a concurrent branch's entries are never dropped.
pub fn mutate(
  id: String,
  f: fn(SessionTree) -> SessionTree,
) -> Result(SessionTree, StoreError) {
  with_lock(id, fn() {
    case session_manager.load(id) {
      Ok(tree) -> {
        let updated = f(tree)
        case session_manager.save(updated) {
          Ok(_) -> Ok(updated)
          Error(e) -> Error(e)
        }
      }
      Error(e) -> Error(e)
    }
  })
}
