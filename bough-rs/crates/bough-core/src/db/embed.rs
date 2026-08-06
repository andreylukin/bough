//! The optional vector layer over the command-history memory (port of
//! `src/history/embed.ts`): local embeddings generated INSIDE SQLite
//! (sqlite-vec + sqlite-lembed), in their OWN database file
//! (`~/.bough/embeddings.db`), never in bough.db.
//!
//! v1 STUB per ARCHITECTURE.md §2: `create_embed_layer()` returns `None` —
//! graceful absence is the documented contract (macOS-without-Homebrew is
//! already this state in TS); tags + FTS carry recall alone, and
//! `bough tags similar` exits 1 with the FTS pointer message. Wave 3.17
//! un-stubs (separate embeddings.db + ATTACH + count-delta drain + model-id
//! rebuild semantics kept).

use crate::errors::BoughError;

pub struct EmbedLayerOptions {
    pub bough_db: Option<String>,
    pub embed_db: Option<String>,
    pub model_path: Option<String>,
}

/// The optional layer. Unreachable in v1 (`create_embed_layer` is always
/// `None`), but the surface is the contract the callers code against.
pub struct EmbedLayer;

impl EmbedLayer {
    /// Index pending command rows; returns how many were embedded (count-delta,
    /// never `changes`). Any error loses one tick, never the layer.
    pub fn drain(&self) -> Result<u64, BoughError> {
        Ok(0)
    }

    /// KNN-10 over the command history. Failure is a catchable, explanatory
    /// rejection.
    pub fn similar(&self, _text: &str) -> Result<Vec<serde_json::Value>, BoughError> {
        Ok(vec![])
    }

    pub fn close(self) {}
}

/// `None` = the feature does not exist — the everyday answer, and the whole
/// answer in v1.
pub fn create_embed_layer(_opts: Option<EmbedLayerOptions>) -> Option<EmbedLayer> {
    None
}
