//! Invariant: a scripted run is DETERMINISTIC and its durable output is indistinguishable in
//! SHAPE from a live one — the same steps, in §5's order, with the same consumed sets. The
//! transcript decides what the model said; it decides nothing about the ledger protocol.

use bough_plugin_llm::{Chunk, LlmFailure, StopReason, ToolCallId, ToolName};

/// The serde spelling of one chunk. `Chunk` itself is not `Deserialize` (it carries a provider
/// `Usage`), so the transcript speaks this and converts — one place where a transcript file and
/// the seam's vocabulary meet, rather than a `serde_json::from_value` at every call site.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "chunk", rename_all = "snake_case")]
pub enum ScriptedChunk {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default)]
        meta: Option<serde_json::Value>,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    End {
        #[serde(default = "end_turn")]
        stop: StopReason,
    },
    Failed {
        failure: LlmFailure,
    },
}

fn end_turn() -> StopReason {
    StopReason::EndTurn
}

impl ScriptedChunk {
    /// The seam's chunk.
    pub fn to_chunk(&self) -> Chunk {
        match self {
            ScriptedChunk::Text { text } => Chunk::TextDelta { text: text.clone() },
            ScriptedChunk::Reasoning { text, meta } => Chunk::ReasoningDelta {
                text: text.clone(),
                meta: meta.clone(),
            },
            ScriptedChunk::ToolCall { id, name, input } => Chunk::ToolCall {
                id: ToolCallId::new(id),
                name: ToolName::new(name),
                input: input.clone(),
            },
            ScriptedChunk::End { stop } => Chunk::End { stop: *stop },
            ScriptedChunk::Failed { failure } => Chunk::Failed(failure.clone()),
        }
    }
}

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
    /// Parse YAML or JSON. YAML is a superset of JSON, so one parser reads both spellings and a
    /// transcript file and an inline `wakes:` block cannot diverge.
    pub fn parse(text: &str) -> Result<Script, String> {
        let script: Script = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
        script.validate()?;
        Ok(script)
    }

    /// Parse the `wakes` value a bundle patch wrote inline.
    pub fn from_value(v: &serde_json::Value) -> Result<Script, String> {
        let wakes: Vec<ScriptedWake> =
            serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        let script = Script { wakes };
        script.validate()?;
        Ok(script)
    }

    /// A transcript that cannot be replayed is a MISCONFIGURATION and fails loud at parse (§0.2),
    /// never half-way through a wake.
    pub fn validate(&self) -> Result<(), String> {
        for (w, wake) in self.wakes.iter().enumerate() {
            for (s, step) in wake.steps.iter().enumerate() {
                let chunks = self
                    .decode(step)
                    .map_err(|e| format!("wake {w} step {s}: {e}"))?;
                match chunks.last() {
                    None => return Err(format!("wake {w} step {s}: no chunks at all")),
                    Some(last) if !last.is_terminal() => {
                        return Err(format!(
                            "wake {w} step {s}: the last chunk is not terminal; a stream carries \
                             exactly one terminal chunk with nothing after it (§12)"
                        ))
                    }
                    Some(_) => {}
                }
                if chunks[..chunks.len() - 1].iter().any(Chunk::is_terminal) {
                    return Err(format!(
                        "wake {w} step {s}: a terminal chunk appears before the end of the stream"
                    ));
                }
            }
        }
        Ok(())
    }

    fn decode(&self, step: &ScriptedStep) -> Result<Vec<Chunk>, String> {
        step.chunks
            .iter()
            .map(|v| {
                serde_json::from_value::<ScriptedChunk>(v.clone())
                    .map(|c| c.to_chunk())
                    .map_err(|e| e.to_string())
            })
            .collect()
    }

    /// The chunks of one step, mapped onto the seam's vocabulary.
    pub fn chunks(&self, wake: usize, step: usize) -> Option<Vec<Chunk>> {
        let step = self.wakes.get(wake)?.steps.get(step)?;
        // `validate` already proved every chunk decodes, and a `Script` is only ever built
        // through a constructor that validates.
        self.decode(step).ok()
    }

    /// How many steps the wake at `wake` scripts. `None` if there is no such wake.
    pub fn steps_in(&self, wake: usize) -> Option<usize> {
        Some(self.wakes.get(wake)?.steps.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_WAKES: &str = r#"
wakes:
  - steps:
      - chunks:
          - { chunk: text, text: "looking at the plan" }
          - { chunk: end, stop: end_turn }
  - steps:
      - chunks:
          - { chunk: reasoning, text: "weighing it" }
          - { chunk: text, text: "done" }
          - { chunk: end, stop: end_turn }
"#;

    #[test]
    fn yaml_and_json_parse_to_the_same_script() {
        let from_yaml = Script::parse(TWO_WAKES).expect("the yaml parses");
        let json = serde_json::to_string(&from_yaml).expect("a script serialises");
        assert_eq!(Script::parse(&json).expect("the json parses"), from_yaml);
        assert_eq!(from_yaml.wakes.len(), 2);
        assert_eq!(from_yaml.steps_in(1), Some(1));
        assert_eq!(from_yaml.steps_in(2), None);
    }

    #[test]
    fn chunks_map_onto_the_seams_vocabulary() {
        let s = Script::parse(TWO_WAKES).unwrap();
        assert_eq!(
            s.chunks(0, 0).unwrap(),
            vec![
                Chunk::TextDelta {
                    text: "looking at the plan".into()
                },
                Chunk::End {
                    stop: StopReason::EndTurn
                },
            ]
        );
        assert!(s.chunks(0, 1).is_none());
    }

    /// §12: exactly one terminal chunk, at the end. A transcript that breaks it is refused at
    /// parse rather than half-way through a wake.
    #[test]
    fn a_stream_without_a_terminal_chunk_is_refused() {
        let err =
            Script::parse("wakes: [ { steps: [ { chunks: [ { chunk: text, text: x } ] } ] } ]")
                .unwrap_err();
        assert!(err.contains("not terminal"), "{err}");
    }

    #[test]
    fn a_terminal_chunk_in_the_middle_is_refused() {
        let err = Script::parse(
            "wakes: [ { steps: [ { chunks: [ { chunk: end }, { chunk: text, text: x }, { chunk: end } ] } ] } ]",
        )
        .unwrap_err();
        assert!(err.contains("before the end"), "{err}");
    }
}
