//! Invariant: a scripted run is DETERMINISTIC and its durable output is indistinguishable in
//! SHAPE from a live one — the same steps, in §5's order, with the same consumed sets. The
//! transcript decides what the model said; it decides nothing about the ledger protocol.

use bough_plugin_llm::Chunk;

/// One scripted step: what the "model" produced.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScriptedStep {
    /// The chunks, in order. The last one is terminal.
    pub chunks: Vec<serde_json::Value>,
}

/// One scripted wake.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScriptedWake {
    pub steps: Vec<ScriptedStep>,
}

/// A whole script.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Script {
    pub wakes: Vec<ScriptedWake>,
}

impl Script {
    /// Parse YAML or JSON. WP-5.
    pub fn parse(_text: &str) -> Result<Script, String> {
        todo!("WP-5")
    }

    /// The chunks of one step, mapped onto the seam's vocabulary. WP-5.
    pub fn chunks(&self, _wake: usize, _step: usize) -> Option<Vec<Chunk>> {
        todo!("WP-5")
    }
}
