//// Owns live sessions. In the OTP design each session is a supervised process
//// (SPEC.md §3); this module is the registry and JSONL persistence surface.

import bough_core/session.{type SessionTree}

/// Persist a session tree to `~/.bough/sessions/<project>/<id>.jsonl`.
pub fn save(_tree: SessionTree) -> Result(Nil, String) {
  Error("session_manager.save: not implemented")
}

/// Load a session tree from disk for `/resume` (SPEC.md §4).
pub fn load(_id: String) -> Result(SessionTree, String) {
  Error("session_manager.load: not implemented")
}
