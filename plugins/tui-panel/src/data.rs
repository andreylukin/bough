//! Invariant: everything here is a PURE join over inputs the pane already gathered — the
//! composition, the kernel snapshot, the seam facts, the ui diff. No I/O, no clock, no seam
//! call, so every row the panel shows is testable from fabricated inputs. Config bodies pass
//! through `bough_kernel::config::render::redact` — the SAME pass the dump uses — before a
//! character reaches a line (a second predicate is how two surfaces disagree about secrets).

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::config::compose::Composition;
use bough_kernel::config::entry::Entry;
use bough_kernel::config::render::redact;
use bough_kernel::RowSnapshot;
use chrono::{DateTime, Utc};

use crate::store::UiEntries;

/// One composed row, flattened with depth, joined with its runtime state.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigRow {
    pub id: String,
    pub depth: usize,
    pub plugin: String,
    /// The EVALUATED `disabled` (§0.5: the composer resolves every expression before the kernel).
    pub disabled: bool,
    /// The fiber state word (`active`, `pending`, `failed`, ...) or `—` for a row the runtime
    /// does not hold (a cascaded-away child of a disabled parent).
    pub state: String,
    pub error: Option<String>,
    pub unmet: Vec<String>,
    /// Which layer created the row, and which last wrote `disabled` / `config` — the column that
    /// turns "my edit did nothing" from a mystery into a fact.
    pub created_by: String,
    pub disabled_by: String,
    pub config_by: String,
    /// What the ui diff currently pins, if anything.
    pub ui_pin: Option<bool>,
    /// A runtime-only mount (`ctx.mount`) has no config row and cannot be toggled by a layer.
    pub runtime_only: bool,
    /// The REDACTED config body, pre-rendered to lines. Empty for a `config: null` row.
    pub config_lines: Vec<String>,
}

impl ConfigRow {
    pub fn toggleable(&self) -> bool {
        !self.runtime_only
    }
}

/// Runtime facts per row id, flattened out of the nested snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapFacts {
    pub state: String,
    pub error: Option<String>,
    pub unmet: Vec<String>,
    pub plugin: String,
}

/// Flatten the snapshot's nested rows into id → facts, depth-first.
pub fn snap_index(rows: &[RowSnapshot]) -> BTreeMap<String, SnapFacts> {
    fn walk(rows: &[RowSnapshot], out: &mut BTreeMap<String, SnapFacts>) {
        for r in rows {
            out.insert(
                r.id.to_string(),
                SnapFacts {
                    state: format!("{:?}", r.state).to_lowercase(),
                    error: r.error.clone(),
                    unmet: r.unmet.clone(),
                    plugin: r.plugin.clone().unwrap_or_default(),
                },
            );
            walk(&r.children, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(rows, &mut out);
    out
}

/// The config tab's rows: the composed tree flattened with depth, each row joined with its
/// runtime facts and its provenance, then every runtime-only mount the snapshot holds that no
/// config row explains, appended flat and marked untoggleable.
pub fn config_rows(
    comp: &Composition,
    snap: &BTreeMap<String, SnapFacts>,
    ui: &UiEntries,
) -> Vec<ConfigRow> {
    fn walk(
        entries: &[Entry],
        depth: usize,
        comp: &Composition,
        snap: &BTreeMap<String, SnapFacts>,
        ui: &UiEntries,
        out: &mut Vec<ConfigRow>,
    ) {
        for e in entries {
            let id = e.id.to_string();
            let prov = comp.provenance.get(&e.id);
            let layer_of = |field: &str| {
                prov.and_then(|p| p.fields.get(field))
                    .or(prov.map(|p| &p.created_by))
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "—".to_string())
            };
            let facts = snap.get(&id);
            out.push(ConfigRow {
                id: id.clone(),
                depth,
                plugin: e.plugin.clone().unwrap_or_default(),
                disabled: matches!(e.disabled, bough_kernel::config::expr::Expr::Literal(true)),
                state: facts.map(|f| f.state.clone()).unwrap_or_else(|| "—".into()),
                error: facts.and_then(|f| f.error.clone()),
                unmet: facts.map(|f| f.unmet.clone()).unwrap_or_default(),
                created_by: prov
                    .map(|p| p.created_by.to_string())
                    .unwrap_or_else(|| "—".into()),
                disabled_by: layer_of("disabled"),
                config_by: layer_of("config"),
                ui_pin: ui.get(&id).copied(),
                runtime_only: false,
                config_lines: config_lines(&e.config),
            });
            walk(&e.group, depth + 1, comp, snap, ui, out);
        }
    }
    let mut out = Vec::new();
    walk(&comp.tree, 0, comp, snap, ui, &mut out);
    let known: BTreeSet<String> = out.iter().map(|r| r.id.clone()).collect();
    for (id, facts) in snap {
        if known.contains(id) {
            continue;
        }
        out.push(ConfigRow {
            id: id.clone(),
            depth: 0,
            plugin: facts.plugin.clone(),
            disabled: false,
            state: facts.state.clone(),
            error: facts.error.clone(),
            unmet: facts.unmet.clone(),
            created_by: "runtime".into(),
            disabled_by: "—".into(),
            config_by: "—".into(),
            ui_pin: None,
            runtime_only: true,
            config_lines: Vec::new(),
        });
    }
    out
}

/// The REDACTED config body as display lines. `null` (no config) is no lines, not `~`.
pub fn config_lines(config: &serde_yaml::Value) -> Vec<String> {
    if matches!(config, serde_yaml::Value::Null) {
        return Vec::new();
    }
    let masked = redact(config.clone());
    match serde_yaml::to_string(&masked) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(e) => vec![format!("(unrenderable config: {e})")],
    }
}

/// The set of ids a toggle may name: every row of the composed tree (§0.5 — a patch targets a
/// row by id, so a runtime-only mount is not in the set).
pub fn known_ids(comp: &Composition) -> BTreeSet<String> {
    fn walk(entries: &[Entry], out: &mut BTreeSet<String>) {
        for e in entries {
            out.insert(e.id.to_string());
            walk(&e.group, out);
        }
    }
    let mut out = BTreeSet::new();
    walk(&comp.tree, &mut out);
    out
}

// ---- connectors ---------------------------------------------------------------------------

/// One MCP server row: the composed row joined with the seam's live facts.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerRow {
    pub name: String,
    /// The owning config row (`mcp.rmcp` / `mcp.subprocess`): the one discriminator between a
    /// remote server and a resident process, and the id a toggle targets.
    pub owner_id: String,
    /// `stdio: npx …` / `http: https://…` / `process: …`, from the composed config.
    pub detail: String,
    pub disabled: bool,
    /// Whether the seam holds it (registration happens only after a first successful connect).
    pub registered: bool,
    /// `McpHandle::is_ready`, when the seam holds the server.
    pub ready: Option<bool>,
    pub tools: Option<usize>,
    /// The child row's state word and error, when the kernel holds a child row for it.
    pub state: String,
    pub error: Option<String>,
}

/// One collector row joined with its schedule job.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectorRow {
    pub id: String,
    pub plugin: String,
    pub disabled: bool,
    /// `every 5m` / `cron <spec>`, from the composed config.
    pub cadence: String,
    /// What the row sweeps, from its own scope fields (repos / teams+projects / queries).
    pub scope: String,
    /// The registered job, when the scheduler holds one for this row.
    pub job: Option<JobFacts>,
}

/// The scheduler's word on one job, already rendered to plain data.
#[derive(Clone, Debug, PartialEq)]
pub struct JobFacts {
    pub name: String,
    pub next: Option<DateTime<Utc>>,
    /// `ran 12:03:11 — 3 delivered from 2 sources` / `pending: …` / `failed: …`.
    pub last: Option<String>,
}

/// Live facts the pane read off the `mcp` seam before calling this.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeamFacts {
    /// name → (ready, tool count).
    pub servers: BTreeMap<String, (Option<bool>, Option<usize>)>,
}

/// The connectors tab's two lists, joined from the composed tree, the kernel snapshot, the seam
/// and the scheduler. Every named collection in the composed `mcp.*` rows appears whether or not
/// it ever connected; every seam registration appears whether or not a row explains it.
pub fn connector_rows(
    comp: &Composition,
    snap: &BTreeMap<String, SnapFacts>,
    seam: &SeamFacts,
    jobs: &[(String, JobFacts)],
) -> (Vec<ServerRow>, Vec<CollectorRow>) {
    let mut servers: Vec<ServerRow> = Vec::new();
    let mut collectors: Vec<CollectorRow> = Vec::new();
    let mut named: BTreeSet<String> = BTreeSet::new();

    fn field<'v>(v: &'v serde_yaml::Value, name: &str) -> Option<&'v serde_yaml::Value> {
        v.get(serde_yaml::Value::String(name.to_string()))
    }
    let count = |v: Option<&serde_yaml::Value>| -> usize {
        match v {
            Some(serde_yaml::Value::Sequence(s)) => s.len(),
            Some(serde_yaml::Value::Mapping(m)) => m.len(),
            _ => 0,
        }
    };

    fn walk<'e>(entries: &'e [Entry], out: &mut Vec<&'e Entry>) {
        for e in entries {
            out.push(e);
            walk(&e.group, out);
        }
    }
    let mut flat = Vec::new();
    walk(&comp.tree, &mut flat);

    for e in &flat {
        let id = e.id.to_string();
        let row_disabled = matches!(e.disabled, bough_kernel::config::expr::Expr::Literal(true));
        let rows_field = if field(&e.config, "servers").is_some() {
            Some(("servers", "transport"))
        } else if field(&e.config, "processes").is_some() {
            Some(("processes", "command"))
        } else {
            None
        };
        if let Some((list_name, detail_field)) = rows_field {
            let Some(serde_yaml::Value::Sequence(list)) = field(&e.config, list_name) else {
                continue;
            };
            for s in list {
                let name = field(s, "name")
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                let entry_disabled = field(s, "disabled")
                    .and_then(serde_yaml::Value::as_bool)
                    .unwrap_or(false);
                let child_id = format!("{id}.{name}");
                let facts = snap.get(&child_id);
                let live = seam.servers.get(&name);
                named.insert(name.clone());
                servers.push(ServerRow {
                    name,
                    owner_id: id.clone(),
                    detail: transport_summary(field(s, detail_field), s),
                    disabled: row_disabled || entry_disabled,
                    registered: live.is_some(),
                    ready: live.and_then(|(r, _)| *r),
                    tools: live.and_then(|(_, t)| *t),
                    state: facts.map(|f| f.state.clone()).unwrap_or_else(|| "—".into()),
                    error: facts.and_then(|f| f.error.clone()),
                });
            }
            continue;
        }
        if id.starts_with("collect.") {
            let cadence = field(&e.config, "cadence")
                .map(cadence_summary)
                .unwrap_or_else(|| "—".into());
            let mut scopes: Vec<String> = Vec::new();
            for (name, word) in [
                ("repos", "repo"),
                ("teams", "team"),
                ("projects", "project"),
                ("queries", "query"),
            ] {
                let n = count(field(&e.config, name));
                if n > 0 {
                    scopes.push(format!("{n} {word}{}", if n == 1 { "" } else { "s" }));
                }
            }
            let job = jobs
                .iter()
                .find(|(owner, _)| owner == &id)
                .map(|(_, j)| j.clone());
            collectors.push(CollectorRow {
                id: id.clone(),
                plugin: e.plugin.clone().unwrap_or_default(),
                disabled: row_disabled,
                cadence,
                scope: if scopes.is_empty() {
                    "nothing configured".into()
                } else {
                    scopes.join(" · ")
                },
                job,
            });
        }
    }

    // A seam registration no composed row names (a test's direct `McpHandle::server`, say) is
    // still shown: the seam is the truth about what can be called.
    for (name, (ready, tools)) in &seam.servers {
        if named.contains(name) {
            continue;
        }
        servers.push(ServerRow {
            name: name.clone(),
            owner_id: "—".into(),
            detail: "registered directly on the seam".into(),
            disabled: false,
            registered: true,
            ready: *ready,
            tools: *tools,
            state: "—".into(),
            error: None,
        });
    }
    (servers, collectors)
}

/// `stdio: <command>` / `http: <url>` / the first string field there is.
fn transport_summary(detail: Option<&serde_yaml::Value>, row: &serde_yaml::Value) -> String {
    let get = |v: &serde_yaml::Value, name: &str| -> Option<String> {
        v.get(serde_yaml::Value::String(name.to_string()))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string)
    };
    match detail {
        Some(serde_yaml::Value::Mapping(_)) => {
            let t = detail.unwrap();
            if let Some(url) = get(t, "url") {
                return format!("http: {url}");
            }
            if let Some(cmd) = get(t, "command") {
                return format!("stdio: {cmd}");
            }
            "?".into()
        }
        Some(serde_yaml::Value::Tagged(tag)) => transport_summary(Some(&tag.value), row),
        Some(serde_yaml::Value::String(s)) => s.clone(),
        _ => get(row, "command")
            .map(|c| format!("process: {c}"))
            .unwrap_or_else(|| "?".into()),
    }
}

/// `every 5m0s` / `cron <spec>`, from the row's own `cadence` field.
fn cadence_summary(v: &serde_yaml::Value) -> String {
    let get = |name: &str| v.get(serde_yaml::Value::String(name.to_string()));
    if let Some(ms) = get("every_ms").and_then(serde_yaml::Value::as_u64) {
        let secs = ms / 1000;
        return if secs % 60 == 0 {
            format!("every {}m", secs / 60)
        } else {
            format!("every {secs}s")
        };
    }
    if let Some(spec) = get("cron").and_then(serde_yaml::Value::as_str) {
        return format!("cron {spec}");
    }
    "—".into()
}

// ---- model ---------------------------------------------------------------------------------

/// One agent's resolution, both ways, computed by `model-policy`'s own `choose` — the panel
/// re-runs the policy rather than re-stating it, so the two cannot drift.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentModelRow {
    pub name: String,
    pub model_override: Option<String>,
    /// What a wake answering Andrey runs on (never overridable).
    pub answer: String,
    /// What an unattended wake runs on.
    pub unattended: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdapterRow {
    pub name: String,
    /// The claim, in the bundle's own spelling: `*`, `openai:*`, an exact id.
    pub claim: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelData {
    /// From the composed `model.policy` row; `None` when no such row is mounted.
    pub sol: Option<String>,
    pub terra: Option<String>,
    pub agents: Vec<AgentModelRow>,
    pub adapters: Vec<AdapterRow>,
    /// Every `api_key_env` name the composed tree mentions, and whether the process env has it.
    /// Presence of the VARIABLE is the most a UI can honestly claim (P2-D7: the key is read at
    /// call time; "authenticated" is unknowable without spending a call).
    pub env_keys: Vec<(String, bool)>,
    /// The last `request/header`'s model — what actually ran, not what config promises.
    pub last_model: Option<String>,
}

/// The `model.policy` row's config, parsed out of the composed tree.
pub fn policy_of(comp: &Composition) -> Option<bough_plugin_model_policy::PolicyConfig> {
    fn find<'e>(entries: &'e [Entry], id: &str) -> Option<&'e Entry> {
        for e in entries {
            if e.id.as_str() == id {
                return Some(e);
            }
            if let Some(hit) = find(&e.group, id) {
                return Some(hit);
            }
        }
        None
    }
    let row = find(&comp.tree, "model.policy")?;
    serde_yaml::from_value(row.config.clone()).ok()
}

/// Re-run the policy for one agent, both ways. The dummy ids are honest: `choose` reads only
/// `answers_andrey` and `model_override` (its own doc), so nothing else in the facts can matter.
pub fn agent_rows(
    cfg: &bough_plugin_model_policy::PolicyConfig,
    agents: &[(String, Option<String>)],
) -> Vec<AgentModelRow> {
    use bough_plugin_llm::{CallConfig, RequestCall, RequestFacts, WakeKind};
    let call = |name: &str, answers: bool, model_override: Option<String>| RequestCall {
        facts: std::sync::Arc::new(RequestFacts {
            agent: bough_plugin_ledger::AgentName::new(name),
            traj: bough_plugin_ledger::TrajId::new("panel/what-if"),
            wake: bough_plugin_ledger::WakeId::new("panel/what-if"),
            wake_kind: if answers {
                WakeKind::Answer
            } else {
                WakeKind::Drain
            },
            step_index: 0,
            answers_andrey: answers,
            model_override,
            prompt_ver: String::new(),
            composition: String::new(),
        }),
        call: CallConfig {
            model: String::new(),
            max_tokens: 0,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    };
    agents
        .iter()
        .map(|(name, over)| AgentModelRow {
            name: name.clone(),
            model_override: over.clone(),
            answer: bough_plugin_model_policy::choose(cfg, &call(name, true, over.clone())),
            unattended: bough_plugin_model_policy::choose(cfg, &call(name, false, over.clone())),
        })
        .collect()
}

/// Every `api_key_env` field in the composed tree, with `is_set` filled by the caller's probe
/// (the builder stays pure; the pane passes `std::env::var(..).is_ok()`).
pub fn env_key_names(comp: &Composition) -> Vec<String> {
    fn walk(entries: &[Entry], out: &mut Vec<String>) {
        for e in entries {
            if let Some(name) = e
                .config
                .get(serde_yaml::Value::String("api_key_env".into()))
                .and_then(serde_yaml::Value::as_str)
            {
                if !out.iter().any(|n| n == name) {
                    out.push(name.to_string());
                }
            }
            walk(&e.group, out);
        }
    }
    let mut out = Vec::new();
    walk(&comp.tree, &mut out);
    out
}

/// Everything one refresh gathered; the tabs render from this and nothing else.
#[derive(Clone, Debug, Default)]
pub struct PanelData {
    pub fingerprint: String,
    pub layers: Vec<String>,
    pub warnings: Vec<String>,
    pub rows: Vec<ConfigRow>,
    /// `bough_kernel::render(&comp, Yaml)`, verbatim — the raw mode and the `y` copy, so the
    /// panel is provably a consumer of `Composition` and not a second formatter of the dump.
    pub raw_dump: String,
    pub servers: Vec<ServerRow>,
    pub collectors: Vec<CollectorRow>,
    pub model: ModelData,
    pub known_ids: BTreeSet<String>,
    pub ui: UiEntries,
    pub taken_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> serde_yaml::Value {
        serde_yaml::from_str(text).unwrap()
    }

    #[test]
    fn config_lines_pass_through_the_dumps_redaction() {
        let lines = config_lines(&yaml("api_key: shhh\napi_key_env: FINE\nmax_tokens: 3"));
        let text = lines.join("\n");
        assert!(!text.contains("shhh"), "{text}");
        assert!(text.contains("«redacted»"), "{text}");
        assert!(text.contains("FINE"), "{text}");
        assert!(text.contains("max_tokens: 3"), "{text}");
    }

    #[test]
    fn a_null_config_is_no_lines() {
        assert!(config_lines(&serde_yaml::Value::Null).is_empty());
    }

    #[test]
    fn transport_summaries_name_the_thing_a_person_would() {
        assert_eq!(
            transport_summary(Some(&yaml("{ url: https://x/mcp }")), &yaml("{}")),
            "http: https://x/mcp"
        );
        assert_eq!(
            transport_summary(Some(&yaml("{ command: npx, args: [x] }")), &yaml("{}")),
            "stdio: npx"
        );
        assert_eq!(
            transport_summary(None, &yaml("{ command: ./server }")),
            "process: ./server"
        );
    }

    #[test]
    fn cadences_render_as_minutes_when_whole() {
        assert_eq!(cadence_summary(&yaml("{ every_ms: 300000 }")), "every 5m");
        assert_eq!(cadence_summary(&yaml("{ every_ms: 90000 }")), "every 90s");
        assert_eq!(
            cadence_summary(&yaml("{ cron: \"0 0 9 * * *\" }")),
            "cron 0 0 9 * * *"
        );
    }

    #[test]
    fn agent_rows_rerun_the_policy_rather_than_restating_it() {
        let cfg = bough_plugin_model_policy::PolicyConfig {
            sol: "sol-model".into(),
            terra: "terra-model".into(),
            prices: Default::default(),
        };
        let rows = agent_rows(
            &cfg,
            &[
                ("sol".into(), None),
                ("terra".into(), Some("special-model".into())),
            ],
        );
        assert_eq!(rows[0].answer, "sol-model");
        assert_eq!(rows[0].unattended, "terra-model");
        // §12: the override applies to unattended wakes only; sol-for-Andrey is not overridable.
        assert_eq!(rows[1].answer, "sol-model");
        assert_eq!(rows[1].unattended, "special-model");
    }
}
