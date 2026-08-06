//! Provider model discovery (catalog listing). v1 STUB (plan rows 1.12 /
//! 2.16+): the picker works from the static `MODELS` table; every discovery
//! answers the contract-shaped honest empty list. The failure policy the
//! full port must keep: discovery **never throws and never caches** — no
//! key, a bad key, a rate limit or an offline machine all mean "no extra
//! rows", and one provider failing must not cost the others their rows.

use crate::llm::routing::{ModelRow, ProviderOpts};

/// Static table first, discovered entries after, deduped by id (static wins).
pub fn merge_models(static_models: &[ModelRow], dynamic: &[ModelRow]) -> Vec<ModelRow> {
    let seen: std::collections::HashSet<&str> =
        static_models.iter().map(|m| m.id.as_str()).collect();
    let mut out: Vec<ModelRow> = static_models.to_vec();
    out.extend(dynamic.iter().filter(|m| !seen.contains(m.id.as_str())).cloned());
    out
}

/// v1: no dynamic rows. The wave-2 port asks all four providers concurrently,
/// each independently fallible to `[]`.
pub async fn discover_models(_opts: ProviderOpts) -> Vec<ModelRow> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::routing::{Provider, MODELS};

    #[test]
    fn merge_models_the_static_table_wins_on_id_collisions() {
        let dynamic = vec![
            ModelRow { id: "openai:gpt-5".into(), label: "gpt-5 (OpenAI)".into(), provider: Provider::Openai },
            ModelRow { id: "openai:o4".into(), label: "o4 (OpenAI)".into(), provider: Provider::Openai },
        ];
        let merged = merge_models(&MODELS, &dynamic);
        assert_eq!(merged.iter().filter(|m| m.id == "openai:gpt-5").count(), 1);
        assert_eq!(merged.last().unwrap().id, "openai:o4");
        // The static entry's label survived the collision.
        let gpt5 = merged.iter().find(|m| m.id == "openai:gpt-5").unwrap();
        assert_eq!(gpt5.label, "GPT-5 (OpenAI)");
    }

    #[tokio::test]
    async fn discovery_stub_answers_the_honest_empty_list() {
        assert!(discover_models(ProviderOpts::default()).await.is_empty());
    }
}
