//! Structured agent output — the reliability mechanism under fan-out (port of
//! `src/workflow/schema.ts`).
//!
//! A workflow's only machine-readable product is what its agents report, and
//! there is no acceptance gate anywhere in bough: the model says what it did and
//! the user verifies. So a 300-agent audit that reports prose gives the script
//! nothing to branch on but string matching, and one agent that decides to
//! answer in a table silently changes the shape of the whole run.
//! `agent(prompt, {schema})` is the fix — the call resolves to a PARSED,
//! VALIDATED object and the script branches on typed data.
//!
//! THE INVARIANT THIS HOLDS: **a schema mismatch retries; an exhausted retry
//! fails the call.** `agent()` either resolves with an object that validates
//! against the supplied schema or it throws — it never resolves with junk. That
//! matters more than it sounds: `parallel()` maps a throwing thunk to `null` and
//! `pipeline()` drops the item, so a *thrown* schema failure is visible in the
//! result and is handled by combinators the script already uses, whereas a
//! malformed object resolved into a slot propagates as a `TypeError` three
//! stages later, in a detached run nobody is watching.
//!
//! Two consequences shape the code below:
//!
//!   - The schema itself is rejected BEFORE the first subagent launches. An
//!     unsupported schema is an authoring mistake, and finding out mid-run —
//!     after forty agents have billed — is the expensive way to learn it.
//!   - Success returns the CANONICAL JSON text of the validated value, not the
//!     subagent's raw report. The worker does `JSON.parse(report)` when `schema`
//!     is set and the engine journals the same string, so a replayed call and a
//!     live one must be byte-identical to the script. Handing back a fenced
//!     markdown block would work live and fail on replay.
//!
//! The one deliberate divergence from the SDKs: they silently STRIP unsupported
//! constraints and check them client-side, where this rejects them by name at
//! submit. Stripping is the wrong trade for a detached fan-out — a `minItems: 3`
//! that quietly stops constraining the model is a script whose author believes
//! something about the data that is not true.
//!
//! Everything here is pure except [`StructuredRunner`], which decorates the
//! injected [`AgentRunner`] and is therefore drivable offline with no LLM.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::errors::{BoughError, ErrorKind};

use super::key::clip;
use super::runner::{AgentCall, AgentRunner, OnSpawned};

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Attempts per schema-bearing `agent()` call, INCLUDING the first — 3 means one
/// try and two retries. Each attempt is a whole subagent turn, so this is a real
/// multiplier on a fan-out's bill; two retries is the point where "the model
/// slipped" has been ruled out and the schema is the more likely suspect.
pub const DEFAULT_ATTEMPTS: usize = 3;

/// Env-overridable so the exhaustion path is testable without three real turns.
pub fn structured_attempts() -> usize {
    match std::env::var("BOUGH_SCHEMA_ATTEMPTS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        Some(n) if n.is_finite() && n >= 1.0 => n.floor() as usize,
        _ => DEFAULT_ATTEMPTS,
    }
}

/// How many validation errors travel back to the model, and into the final error.
pub const MAX_ERRORS: usize = 12;

/// How much of a malformed report is quoted back. Enough to see the shape.
const REPORT_CLIP: usize = 800;

// ---------------------------------------------------------------------------
// Schema validation (pure) — the submit-time gate
// ---------------------------------------------------------------------------

fn is_obj(v: &Value) -> bool {
    v.is_object()
}

/// The seven types structured-output schemas accept.
const TYPES: [&str; 7] = [
    "object", "array", "string", "integer", "number", "boolean", "null",
];

/// Structural keywords this validator understands, in the order the message
/// lists them.
const SUPPORTED: [&str; 12] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "anyOf",
    "allOf",
    "$ref",
    "$defs",
    "definitions",
];

/// Annotations: carried to the model in the contract, never enforced.
const ANNOTATIONS: [&str; 8] = [
    "title",
    "description",
    "default",
    "examples",
    "format",
    "$comment",
    "$schema",
    "$id",
];

/// Keywords rejected by name rather than ignored, with the reason. Each message
/// says the move, because "unsupported" alone leaves the author guessing whether
/// to drop the keyword or the whole approach — error text is a product surface.
fn rejected_reason(key: &str) -> Option<&'static str> {
    Some(match key {
        "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" => {
            "numeric bounds are not supported"
        }
        "minLength" | "maxLength" => "string-length bounds are not supported",
        "pattern" => "regex constraints are not supported",
        "minItems" | "maxItems" => "array-length bounds are not supported",
        "uniqueItems" => "array uniqueness is not supported",
        "contains" | "minContains" | "maxContains" => "array `contains` is not supported",
        "prefixItems" => "tuple schemas are not supported",
        "oneOf" => "`oneOf` is not supported — use `anyOf`",
        "not" => "`not` is not supported",
        "if" | "then" | "else" => "conditional schemas are not supported",
        "patternProperties" => "`patternProperties` is not supported",
        "propertyNames" => "`propertyNames` is not supported",
        "dependentSchemas" | "dependentRequired" => "schema dependencies are not supported",
        "unevaluatedProperties" => "`unevaluatedProperties` is not supported",
        "unevaluatedItems" => "`unevaluatedItems` is not supported",
        _ => return None,
    })
}

const ADVICE: &str = "The model is not constrained by it, so leaving it in would promise the \
                      script something the schema cannot deliver. Drop the keyword and check \
                      the value in the script instead.";

/// Check a script's JSON Schema against the subset structured outputs accept.
/// Returns the message to hand the author, or `None` when the schema is usable.
///
/// Pure, and separate from [`assert_output_schema`] so a caller can look before
/// it leaps — a route or a linter can report every schema in a script without
/// failing.
pub fn check_output_schema(schema: &Value) -> Option<String> {
    if !is_obj(schema) {
        return Some(
            "agent(prompt, {schema}): schema must be a JSON Schema object — e.g. \
             {type: \"object\", properties: {…}, required: […], additionalProperties: false}"
                .to_string(),
        );
    }
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Some(
            "agent(prompt, {schema}): the schema's root must be `type: \"object\"`. A bare \
             array or scalar root is not accepted; wrap it, e.g. {type: \"object\", \
             properties: {items: {type: \"array\", items: …}}, required: [\"items\"], \
             additionalProperties: false}."
                .to_string(),
        );
    }
    match walk_schema(schema, "", schema, &[]) {
        Ok(()) => None,
        Err(msg) => Some(format!("agent(prompt, {{schema}}): {msg}")),
    }
}

/// Reject an unusable schema at SUBMIT time — before a subagent is launched,
/// before a semaphore slot is taken, before anything bills.
pub fn assert_output_schema(schema: &Value) -> Result<(), BoughError> {
    match check_output_schema(schema) {
        None => Ok(()),
        Some(bad) => Err(BoughError::http(400, ErrorKind::Workflow, bad)),
    }
}

fn reject(path: &str, message: String) -> Result<(), String> {
    Err(if path.is_empty() {
        message
    } else {
        format!("{message} (at `{path}`)")
    })
}

fn describe(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Array(_) => "an array".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(_) => "a string".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

fn defs_of(root: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["definitions", "$defs"] {
        if let Some(Value::Object(m)) = root.get(key) {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

fn ref_name(r: &str, path: &str) -> Result<String, String> {
    let rest = r
        .strip_prefix("#/$defs/")
        .or_else(|| r.strip_prefix("#/definitions/"))
        .filter(|n| !n.is_empty() && !n.contains('/'));
    match rest {
        Some(name) => Ok(name.to_string()),
        None => {
            let _ = reject(
                path,
                format!("`$ref` must be a local reference of the form `#/$defs/Name`; got `{r}`"),
            );
            Err(if path.is_empty() {
                format!("`$ref` must be a local reference of the form `#/$defs/Name`; got `{r}`")
            } else {
                format!(
                    "`$ref` must be a local reference of the form `#/$defs/Name`; got `{r}` \
                     (at `{path}`)"
                )
            })
        }
    }
}

/// `refs` is the `$ref` chain currently being expanded — the recursion detector.
fn walk_schema(node: &Value, path: &str, root: &Value, refs: &[String]) -> Result<(), String> {
    let at = if path.is_empty() {
        "the root".to_string()
    } else {
        path.to_string()
    };
    let Some(map) = node.as_object() else {
        return reject(
            path,
            format!(
                "every subschema must be an object; `{at}` is {}",
                describe(node)
            ),
        );
    };

    for key in map.keys() {
        if let Some(reason) = rejected_reason(key) {
            return reject(
                path,
                format!(
                    "`{key}` is not supported in a structured-output schema — {reason}. {ADVICE}"
                ),
            );
        }
        if !SUPPORTED.contains(&key.as_str()) && !ANNOTATIONS.contains(&key.as_str()) {
            return reject(
                path,
                format!(
                    "unknown schema keyword `{key}`. Supported: {} (plus title/description/\
                     format, which are passed to the model as documentation).",
                    SUPPORTED.join(", ")
                ),
            );
        }
    }

    if let Some(Value::String(r)) = map.get("$ref") {
        let name = ref_name(r, path)?;
        if refs.contains(&name) {
            let chain = refs
                .iter()
                .cloned()
                .chain(std::iter::once(name.clone()))
                .collect::<Vec<_>>();
            return reject(
                path,
                format!(
                    "recursive schema: `$ref` to `{name}` re-enters itself ({}). Structured \
                     outputs cannot express recursion — flatten the shape to a fixed depth, or \
                     return the nesting as a list of nodes with parent ids.",
                    chain.join(" → ")
                ),
            );
        }
        let defs = defs_of(root);
        let Some(target) = defs.get(&name) else {
            return reject(
                path,
                format!(
                    "`$ref` points at `{r}`, which the schema does not define. Add it under \
                     `$defs`."
                ),
            );
        };
        let mut chain = refs.to_vec();
        chain.push(name.clone());
        return walk_schema(target, &format!("{path}/$ref({name})"), root, &chain);
    }

    for key in ["$defs", "definitions"] {
        if let Some(defs) = map.get(key) {
            if !is_obj(defs) {
                return reject(
                    path,
                    format!("`{key}` must be an object of named subschemas"),
                );
            }
        }
    }

    for key in ["anyOf", "allOf"] {
        let Some(branches) = map.get(key) else {
            continue;
        };
        match branches.as_array() {
            Some(list) if !list.is_empty() => {
                for (i, b) in list.iter().enumerate() {
                    walk_schema(b, &format!("{path}/{key}/{i}"), root, refs)?;
                }
            }
            _ => {
                return reject(
                    path,
                    format!("`{key}` must be a non-empty array of subschemas"),
                );
            }
        }
    }

    if let Some(e) = map.get("enum") {
        if e.as_array().is_none_or(|a| a.is_empty()) {
            return reject(
                path,
                "`enum` must be a non-empty array of allowed values".to_string(),
            );
        }
    }

    let Some(ty) = map.get("type") else {
        // A pure combinator node (anyOf/allOf/enum/const) is fine; a node with
        // no constraint at all would validate anything, which is not a schema.
        let constrained = ["anyOf", "allOf", "enum", "const"]
            .iter()
            .any(|k| map.contains_key(*k));
        if !constrained {
            return reject(
                path,
                format!(
                    "`{at}` declares no `type` — an unconstrained subschema accepts anything, \
                     which defeats the point of passing a schema"
                ),
            );
        }
        return Ok(());
    };
    if ty.is_array() {
        return reject(
            path,
            "a `type` array is not supported — express a union with `anyOf`, e.g. anyOf: \
             [{type: \"string\"}, {type: \"null\"}]"
                .to_string(),
        );
    }
    let Some(ty) = ty.as_str().filter(|t| TYPES.contains(t)) else {
        return reject(
            path,
            format!(
                "unknown `type`: {}. One of {}.",
                serde_json::to_string(&map["type"]).unwrap_or_default(),
                TYPES.join(", ")
            ),
        );
    };

    if ty == "object" {
        if map.get("additionalProperties") != Some(&Value::Bool(false)) {
            let says = match map.get("additionalProperties") {
                None => "omits it".to_string(),
                Some(v) => format!(
                    "sets it to {}",
                    serde_json::to_string(v).unwrap_or_default()
                ),
            };
            return reject(
                path,
                format!(
                    "every object must set `additionalProperties: false` — `{at}` {says}. A \
                     closed object is what makes an extra invented field a validation failure \
                     instead of silent noise in the result."
                ),
            );
        }
        let props = map.get("properties");
        let Some(props) = props.and_then(Value::as_object).filter(|p| !p.is_empty()) else {
            return reject(path, format!("`{at}` is an object with no `properties`"));
        };
        if let Some(required) = map.get("required") {
            let Some(list) = required
                .as_array()
                .filter(|l| l.iter().all(Value::is_string))
            else {
                return reject(
                    path,
                    "`required` must be an array of property names".to_string(),
                );
            };
            for name in list {
                let name = name.as_str().unwrap_or_default();
                if !props.contains_key(name) {
                    return reject(
                        path,
                        format!("`required` names `{name}`, which is not in `properties`"),
                    );
                }
            }
        }
        for (name, sub) in props {
            walk_schema(sub, &format!("{path}/{name}"), root, refs)?;
        }
        return Ok(());
    }

    if ty == "array" {
        let Some(items) = map.get("items") else {
            return reject(
                path,
                format!(
                    "`{at}` is an array with no `items` schema — say what the elements are, or \
                     the script gets a list of anything"
                ),
            );
        };
        if items.is_array() {
            return reject(
                path,
                "an `items` array (tuple form) is not supported — use one `items` schema"
                    .to_string(),
            );
        }
        walk_schema(items, &format!("{path}/items"), root, refs)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Instance validation (pure) — what the retry loop branches on
// ---------------------------------------------------------------------------

/// Validate a parsed value against a schema [`check_output_schema`] accepted.
/// Returns the errors, most useful first, capped at [`MAX_ERRORS`] — the list is
/// fed back to the model verbatim on the retry, and forty near-identical
/// "missing field" lines teach it less than the first few do while costing real
/// context.
///
/// Paths are JSON-pointer shaped (`/findings/0/title`) because the model has to
/// locate the fault in its own output from this string alone.
pub fn validate_instance(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if !is_obj(schema) {
        return vec!["the schema is not an object".to_string()];
    }
    check(schema, value, "", schema, &mut errors);
    errors.truncate(MAX_ERRORS);
    errors
}

fn show(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Array(a) => format!("an array ({} items)", a.len()),
        Value::Object(_) => "an object".to_string(),
        other => clip(&serde_json::to_string(other).unwrap_or_default(), 60),
    }
}

fn type_matches(ty: &str, value: &Value) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value
            .as_f64()
            .is_some_and(|n| n.fract() == 0.0 && n.is_finite()),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn check(node: &Value, value: &Value, path: &str, root: &Value, errors: &mut Vec<String>) {
    if errors.len() >= MAX_ERRORS {
        return;
    }
    let at = if path.is_empty() { "/" } else { path };
    let Some(map) = node.as_object() else { return };

    if let Some(Value::String(r)) = map.get("$ref") {
        let name = r
            .strip_prefix("#/$defs/")
            .or_else(|| r.strip_prefix("#/definitions/"))
            .filter(|n| !n.is_empty() && !n.contains('/'));
        if let Some(target) = name.and_then(|n| defs_of(root).get(n).cloned()) {
            if is_obj(&target) {
                check(&target, value, path, root, errors);
            }
        }
        return;
    }

    if let Some(Value::Array(branches)) = map.get("allOf") {
        for branch in branches {
            if is_obj(branch) {
                check(branch, value, path, root, errors);
            }
        }
    }

    if let Some(Value::Array(branches)) = map.get("anyOf") {
        let matched = branches.iter().any(|branch| {
            if !is_obj(branch) {
                return false;
            }
            let mut sub = Vec::new();
            check(branch, value, path, root, &mut sub);
            sub.is_empty()
        });
        if !matched {
            errors.push(format!(
                "`{at}`: matched none of the {} allowed shapes",
                branches.len()
            ));
            return;
        }
    }

    if let Some(konst) = map.get("const") {
        if konst != value {
            errors.push(format!(
                "`{at}`: expected the constant {}, got {}",
                serde_json::to_string(konst).unwrap_or_default(),
                show(value)
            ));
            return;
        }
    }

    if let Some(Value::Array(allowed)) = map.get("enum") {
        if !allowed.iter().any(|a| a == value) {
            errors.push(format!(
                "`{at}`: {} is not one of {}",
                show(value),
                allowed
                    .iter()
                    .map(|e| serde_json::to_string(e).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
    }

    let Some(ty) = map.get("type").and_then(Value::as_str) else {
        return;
    };

    if !type_matches(ty, value) {
        errors.push(format!("`{at}`: expected {ty}, got {}", show(value)));
        return;
    }

    if ty == "object" {
        let obj = value.as_object().expect("type matched");
        let empty = Map::new();
        let props = map
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        if let Some(Value::Array(required)) = map.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !obj.contains_key(name) {
                    errors.push(format!("`{at}`: missing required property `{name}`"));
                    if errors.len() >= MAX_ERRORS {
                        return;
                    }
                }
            }
        }
        if map.get("additionalProperties") == Some(&Value::Bool(false)) {
            for name in obj.keys() {
                if !props.contains_key(name) {
                    errors.push(format!(
                        "`{at}`: unexpected property `{name}` — the schema declares only {}",
                        props
                            .keys()
                            .map(|p| format!("`{p}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    if errors.len() >= MAX_ERRORS {
                        return;
                    }
                }
            }
        }
        for (name, sub) in props {
            let Some(child) = obj.get(name) else { continue };
            if !is_obj(sub) {
                continue;
            }
            check(sub, child, &format!("{path}/{name}"), root, errors);
            if errors.len() >= MAX_ERRORS {
                return;
            }
        }
        return;
    }

    if ty == "array" {
        if let Some(items) = map.get("items").filter(|i| is_obj(i)) {
            for (i, item) in value.as_array().expect("type matched").iter().enumerate() {
                check(items, item, &format!("{path}/{i}"), root, errors);
                if errors.len() >= MAX_ERRORS {
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reading JSON back out of a report (pure)
// ---------------------------------------------------------------------------

/// Find the JSON value in a subagent's report.
///
/// A subagent's report is the final TEXT of a whole turn, so even a perfectly
/// compliant agent commonly wraps its answer in a fenced block, and a chatty one
/// puts a sentence in front of it. Insisting on a bare JSON body would burn
/// retries on agents that got the data right, so this reads the LAST complete
/// JSON value in the report — the last one being the conclusion, where an
/// earlier one is usually an example the agent was quoting from its own
/// instructions.
pub fn extract_json(report: &str) -> Option<Value> {
    let text = report.trim();
    if text.is_empty() {
        return None;
    }

    // The whole report, when the agent did exactly what it was asked.
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }

    // Fenced blocks, last first.
    static FENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let fence = FENCE.get_or_init(|| {
        regex::Regex::new(r"(?is)```(?:json|jsonc|json5)?[ \t]*\r?\n(.*?)```").expect("static")
    });
    let fences: Vec<String> = fence
        .captures_iter(text)
        .map(|c| c[1].trim().to_string())
        .collect();
    for body in fences.iter().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            return Some(v);
        }
    }

    // Anything balanced left in the prose.
    let spans = balanced_spans(text);
    for (start, end) in spans.iter().rev() {
        let chars: Vec<char> = text.chars().collect();
        let candidate: String = chars[*start..*end].iter().collect();
        if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
            return Some(v);
        }
    }
    None
}

/// Top-level `{…}` / `[…]` spans (char offsets), string- and escape-aware.
/// Nested braces are skipped rather than reported, so a candidate is always an
/// outermost value.
fn balanced_spans(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let open = chars[i];
        if open != '{' && open != '[' {
            i += 1;
            continue;
        }
        let close = if open == '{' { '}' } else { ']' };
        let mut depth = 0i64;
        let mut in_string = false;
        let mut escaped = false;
        let mut j = i;
        while j < chars.len() {
            let c = chars[j];
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                j += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
            } else if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    spans.push((i, j + 1));
                    i = j;
                    break;
                }
            }
            j += 1;
        }
        i += 1;
    }
    spans
}

// ---------------------------------------------------------------------------
// The prompt contract (pure)
// ---------------------------------------------------------------------------

/// What is appended to a schema-bearing prompt. A workflow agent gets no context
/// beyond its prompt string, so if the contract is not in the prompt the agent
/// has no way to know one exists — and an agent that answers the question
/// perfectly in prose has still failed the call.
pub fn schema_contract(schema: &Value) -> String {
    [
        "",
        "---",
        "RETURN FORMAT — required, and checked.",
        "",
        "Finish your report with exactly one JSON value that validates against the schema",
        "below, and write nothing after it. A ```json fenced block is fine. Every object in",
        "the schema is closed, so an extra field you invented fails the whole report; include",
        "every required field, and when you could not determine a value say so inside the",
        "structure the schema gives you rather than dropping the field or answering in prose.",
        "",
        "JSON Schema:",
        "```json",
        &serde_json::to_string_pretty(schema).unwrap_or_else(|_| "{}".to_string()),
        "```",
    ]
    .join("\n")
}

/// What is appended on a retry. It names what failed and quotes the report back,
/// because the agent that produced it is a FRESH session with no memory of the
/// previous attempt — a bare "try again" would re-run the same task with the
/// same information and get the same answer.
pub fn repair_contract(previous: &str, errors: &[String], attempt: usize) -> String {
    let mut lines: Vec<String> = vec![
        String::new(),
        "---".to_string(),
        format!("PREVIOUS ATTEMPT REJECTED (attempt {attempt})."),
        String::new(),
        "An earlier agent was given this exact task and its report did not match the schema:"
            .to_string(),
        String::new(),
    ];
    lines.extend(errors.iter().map(|e| format!("  - {e}")));
    lines.extend([
        String::new(),
        "Its report began:".to_string(),
        "```".to_string(),
        clip(previous.trim(), REPORT_CLIP),
        "```".to_string(),
        String::new(),
        "Do the work again and return a report that satisfies every point above. The schema"
            .to_string(),
        "is not negotiable — if the task genuinely cannot produce a required field, fill it"
            .to_string(),
        "with the schema-legal value that says so and explain inside the structure.".to_string(),
    ]);
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// The runner decorator
// ---------------------------------------------------------------------------

/// Wrap an [`AgentRunner`] so `{schema}` calls resolve to validated, canonical
/// JSON.
///
/// Calls WITHOUT a schema pass straight through untouched — a workflow mixes
/// both, and a prose report must not be second-guessed.
///
/// Failure semantics, which the combinators depend on:
///   - Unusable schema → `WorkflowError(400)` before anything launches.
///   - Mismatch → retry with the errors fed back, up to `attempts`.
///   - Exhausted → `WorkflowError(422)` naming the attempts, the last errors and
///     the move. It FAILS rather than resolving, so `parallel()` slots it `null`
///     and `pipeline()` drops the item.
///   - The inner runner itself failing (child errored, interrupted, orphaned, or
///     the run was stopped) is NOT retried and propagates as-is. That is a
///     different failure from "the report was the wrong shape", and retrying it
///     would spend a stopped run's budget and hide an interrupt.
pub struct StructuredRunner {
    inner: Arc<dyn AgentRunner>,
    attempts: usize,
}

impl StructuredRunner {
    pub fn new(inner: Arc<dyn AgentRunner>, attempts: Option<usize>) -> StructuredRunner {
        StructuredRunner {
            inner,
            attempts: attempts.unwrap_or_else(structured_attempts).max(1),
        }
    }
}

#[async_trait]
impl AgentRunner for StructuredRunner {
    async fn run(
        &self,
        call: &AgentCall,
        cancel: CancellationToken,
        on_spawned: OnSpawned,
    ) -> Result<String, BoughError> {
        let Some(schema) = call.schema.clone() else {
            return self.inner.run(call, cancel, on_spawned).await;
        };

        // Submit time: before the first launch, before a semaphore slot, before
        // cost.
        assert_output_schema(&schema)?;

        let contract = schema_contract(&schema);
        let mut previous = String::new();
        let mut errors: Vec<String> = Vec::new();

        for attempt in 1..=self.attempts {
            if cancel.is_cancelled() {
                return Err(BoughError::http(
                    409,
                    ErrorKind::Workflow,
                    "workflow stopped",
                ));
            }
            let prompt = if attempt == 1 {
                format!("{}\n{contract}", call.prompt)
            } else {
                format!(
                    "{}\n{contract}\n{}",
                    call.prompt,
                    repair_contract(&previous, &errors, attempt - 1)
                )
            };
            let attempt_call = AgentCall {
                prompt,
                ..call.clone()
            };
            let report = self
                .inner
                .run(&attempt_call, cancel.clone(), on_spawned.clone())
                .await?;

            let Some(found) = extract_json(&report) else {
                previous = report;
                errors = vec![
                    "the report contained no JSON value at all — the whole answer was prose"
                        .to_string(),
                ];
                continue;
            };

            let bad = validate_instance(&schema, &found);
            if bad.is_empty() {
                // CANONICAL text, not the raw report: the worker parses this and
                // the journal replays it, so live and replayed calls must be
                // identical.
                //
                // NOTE (Rust delta): `serde_json::Map` is a BTreeMap here, so
                // the canonical form sorts object keys where `JSON.stringify`
                // kept parse order. Within one engine it is still one text per
                // value — which is what live-vs-replayed byte-identity needs —
                // and a journal written by the TS engine replays verbatim from
                // its stored string, so nothing re-runs across the cutover.
                return Ok(serde_json::to_string(&found).unwrap_or_else(|_| "null".to_string()));
            }
            previous = report;
            errors = bad;
        }

        Err(BoughError::http(
            422,
            ErrorKind::Workflow,
            format!(
                "agent(prompt, {{schema}}) failed after {} attempt(s): the subagent's report \
                 never matched the schema. Last mismatch:\n{}\nLast report began: {}\nA schema \
                 the agent cannot satisfy is usually asking for something the task has no way \
                 to know — simplify the schema, split the work, or say in the prompt where the \
                 agent should look for the missing field.",
                self.attempts,
                errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                clip(
                    if previous.trim().is_empty() {
                        "(empty)"
                    } else {
                        previous.trim()
                    },
                    300
                ),
            ),
        ))
    }
}

/// Apply the decorator to a workflow context. The boot seam
/// (`ctx.workflow_ctx`) reads as `decorate` in the control layer, so every
/// `WorkflowCtx` built in this process gets structured output without the call
/// site having to remember.
pub fn structured_workflow_ctx(
    base: super::engine::WorkflowCtx,
    attempts: Option<usize>,
) -> super::engine::WorkflowCtx {
    let runner: Arc<dyn AgentRunner> =
        Arc::new(StructuredRunner::new(base.runner.clone(), attempts));
    super::engine::WorkflowCtx { runner, ..base }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::runner::{no_spawn_hook, FnRunner};
    use serde_json::json;
    use std::sync::Mutex;

    fn finding_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "severity": {"enum": ["low", "high"]},
                        },
                        "required": ["title", "severity"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["findings"],
            "additionalProperties": false,
        })
    }

    // ---- the submit gate --------------------------------------------------

    /// An ACCEPTED schema passes first — the row-3.11 gate. The keys already
    /// hash the schema opaquely, so a journal written before this module
    /// existed stays valid.
    #[test]
    fn an_accepted_schema_is_accepted() {
        assert_eq!(check_output_schema(&finding_schema()), None);
        assert!(assert_output_schema(&finding_schema()).is_ok());
    }

    /// Unsupported keywords are REJECTED BY NAME with the move, never stripped.
    #[test]
    fn unsupported_keywords_are_named_with_the_move() {
        let mut s = finding_schema();
        s["properties"]["findings"]["minItems"] = json!(3);
        let msg = check_output_schema(&s).expect("minItems is refused");
        assert!(msg.contains("`minItems` is not supported"), "{msg}");
        assert!(msg.contains("array-length bounds"), "{msg}");
        assert!(
            msg.contains("check the value in the script instead"),
            "{msg}"
        );
        assert!(msg.contains("/findings"), "{msg}");
    }

    #[test]
    fn the_structural_rules_each_say_what_to_do() {
        // Root must be an object.
        let msg = check_output_schema(&json!({"type": "array", "items": {"type": "string"}}))
            .expect("a bare array root is refused");
        assert!(msg.contains("root must be `type: \"object\"`"), "{msg}");

        // Every object closed.
        let open = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
        });
        let msg = check_output_schema(&open).expect("an open object is refused");
        assert!(msg.contains("additionalProperties: false"), "{msg}");

        // `oneOf` names the replacement.
        let mut s = finding_schema();
        s["properties"]["findings"] = json!({"oneOf": [{"type": "string"}]});
        assert!(check_output_schema(&s).unwrap().contains("use `anyOf`"));

        // A type array names the replacement too.
        let mut s = finding_schema();
        s["properties"]["findings"] = json!({"type": ["string", "null"]});
        assert!(check_output_schema(&s)
            .unwrap()
            .contains("express a union with `anyOf`"));

        // Recursion is named, with the cycle.
        let recursive = json!({
            "type": "object",
            "properties": {"node": {"$ref": "#/$defs/Node"}},
            "required": ["node"],
            "additionalProperties": false,
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"child": {"$ref": "#/$defs/Node"}},
                    "additionalProperties": false,
                },
            },
        });
        let msg = check_output_schema(&recursive).expect("recursion is refused");
        assert!(msg.contains("recursive schema"), "{msg}");
        assert!(msg.contains("Node → Node"), "{msg}");

        // A dangling ref says where to put it.
        let dangling = json!({
            "type": "object",
            "properties": {"node": {"$ref": "#/$defs/Missing"}},
            "required": ["node"],
            "additionalProperties": false,
        });
        assert!(check_output_schema(&dangling)
            .unwrap()
            .contains("does not define"));
    }

    // ---- instance validation ----------------------------------------------

    #[test]
    fn instance_errors_are_json_pointers_and_name_the_fault() {
        let value = json!({"findings": [{"title": 7, "severity": "medium", "extra": true}]});
        let errors = validate_instance(&finding_schema(), &value);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("`/findings/0/title`: expected string")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("`/findings/0/severity`") && e.contains("not one of")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("unexpected property `extra`")),
            "{errors:?}"
        );
        assert!(validate_instance(&finding_schema(), &json!({"findings": []})).is_empty());
        // Missing required is named, not silently accepted.
        let errors = validate_instance(&finding_schema(), &json!({}));
        assert!(
            errors[0].contains("missing required property `findings`"),
            "{errors:?}"
        );
    }

    #[test]
    fn errors_are_capped_so_a_retry_still_teaches_something() {
        let mut findings = Vec::new();
        for _ in 0..40 {
            findings.push(json!({"title": 1, "severity": "nope"}));
        }
        let errors = validate_instance(&finding_schema(), &json!({"findings": findings}));
        assert_eq!(errors.len(), MAX_ERRORS);
    }

    // ---- reading JSON out of a report -------------------------------------

    #[test]
    fn the_last_json_value_is_the_conclusion() {
        // Bare.
        assert_eq!(extract_json(r#"{"a":1}"#), Some(json!({"a": 1})));
        // Fenced, with prose in front, and an EARLIER example that must lose.
        let report = "Here is the shape I was asked for:\n```json\n{\"example\": true}\n```\n\
                      and here is my answer:\n```json\n{\"a\": 2}\n```\n";
        assert_eq!(extract_json(report), Some(json!({"a": 2})));
        // Unfenced, in prose, last one wins.
        let report = "first {\"a\": 1} then finally {\"a\": 9} done";
        assert_eq!(extract_json(report), Some(json!({"a": 9})));
        // Prose only.
        assert_eq!(
            extract_json("I looked at the handlers and they seem fine."),
            None
        );
        assert_eq!(extract_json("   "), None);
    }

    // ---- the decorator ----------------------------------------------------

    fn call_with(schema: Option<Value>) -> AgentCall {
        AgentCall {
            prompt: "audit the handlers".into(),
            label: "audit".into(),
            phase: None,
            model: None,
            schema,
        }
    }

    /// Recording inner runner: returns the scripted reports in order and keeps
    /// every prompt it was given.
    fn scripted(
        reports: Vec<Result<String, BoughError>>,
    ) -> (Arc<dyn AgentRunner>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(
            reports
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let sink = seen.clone();
        let runner: Arc<dyn AgentRunner> = Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
            let sink = sink.clone();
            let queue = queue.clone();
            async move {
                sink.lock().unwrap().push(call.prompt.clone());
                queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Ok("no more scripted reports".to_string()))
            }
        }));
        (runner, seen)
    }

    /// A call with NO schema passes through untouched — prompt included.
    #[tokio::test]
    async fn a_call_without_a_schema_passes_straight_through() {
        let (inner, seen) = scripted(vec![Ok("a prose report".into())]);
        let runner = StructuredRunner::new(inner, Some(3));
        let out = runner
            .run(&call_with(None), CancellationToken::new(), no_spawn_hook())
            .await
            .unwrap();
        assert_eq!(out, "a prose report");
        assert_eq!(
            seen.lock().unwrap()[0],
            "audit the handlers",
            "the prompt is untouched"
        );
    }

    /// The happy path returns CANONICAL text, not the fenced report — live and
    /// replayed calls must be byte-identical to the script.
    #[tokio::test]
    async fn a_valid_report_returns_canonical_json_and_carries_the_contract() {
        let (inner, seen) = scripted(vec![Ok(
            "Done.\n```json\n{\n  \"findings\": [ {\"title\": \"a\", \"severity\": \"low\"} ]\n}\n```"
                .into(),
        )]);
        let runner = StructuredRunner::new(inner, Some(3));
        let out = runner
            .run(
                &call_with(Some(finding_schema())),
                CancellationToken::new(),
                no_spawn_hook(),
            )
            .await
            .unwrap();
        assert_eq!(out, r#"{"findings":[{"severity":"low","title":"a"}]}"#);
        let prompt = &seen.lock().unwrap()[0];
        assert!(prompt.starts_with("audit the handlers"), "{prompt}");
        assert!(
            prompt.contains("RETURN FORMAT — required, and checked."),
            "{prompt}"
        );
        assert!(prompt.contains("JSON Schema:"), "{prompt}");
    }

    /// The row-3.11 gate's second half: retry, then fail. A mismatch retries
    /// with the errors and the prior report fed back; an exhausted retry throws
    /// 422 rather than resolving with junk.
    #[tokio::test]
    async fn a_mismatch_retries_with_the_errors_and_an_exhausted_retry_fails_422() {
        // Two bad attempts, then a good one.
        let (inner, seen) = scripted(vec![
            Ok("no json here at all".into()),
            Ok(r#"{"findings": [{"title": 1, "severity": "low"}]}"#.into()),
            Ok(r#"{"findings": [{"title": "ok", "severity": "high"}]}"#.into()),
        ]);
        let runner = StructuredRunner::new(inner, Some(3));
        let out = runner
            .run(
                &call_with(Some(finding_schema())),
                CancellationToken::new(),
                no_spawn_hook(),
            )
            .await
            .unwrap();
        assert_eq!(out, r#"{"findings":[{"severity":"high","title":"ok"}]}"#);

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(
            prompts.len(),
            3,
            "one attempt per failure, including the first"
        );
        assert!(!prompts[0].contains("PREVIOUS ATTEMPT REJECTED"));
        assert!(
            prompts[1].contains("PREVIOUS ATTEMPT REJECTED (attempt 1)"),
            "{}",
            prompts[1]
        );
        assert!(
            prompts[1].contains("no JSON value at all"),
            "{}",
            prompts[1]
        );
        // The retry quotes the prior report — the agent is a fresh session.
        assert!(prompts[2].contains("no json here at all") || prompts[2].contains("\"title\": 1"));

        // Now exhaust them.
        let (inner, seen) = scripted(vec![
            Ok("prose".into()),
            Ok("prose".into()),
            Ok("prose".into()),
            Ok(r#"{"findings": []}"#.into()),
        ]);
        let runner = StructuredRunner::new(inner, Some(3));
        let err = runner
            .run(
                &call_with(Some(finding_schema())),
                CancellationToken::new(),
                no_spawn_hook(),
            )
            .await
            .expect_err("an exhausted retry fails the call");
        assert_eq!(err.status(), 422);
        assert!(
            err.to_string().contains("failed after 3 attempt(s)"),
            "{err}"
        );
        assert!(
            err.to_string().contains("Last report began: prose"),
            "{err}"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            3,
            "the fourth report was never asked for"
        );
    }

    /// The inner runner failing is a DIFFERENT failure and is not retried —
    /// retrying it would spend a stopped run's budget and hide an interrupt.
    #[tokio::test]
    async fn an_inner_failure_is_not_retried() {
        let (inner, seen) = scripted(vec![
            Err(BoughError::http(
                424,
                ErrorKind::Workflow,
                "workflow agent \"audit\" error",
            )),
            Ok(r#"{"findings": []}"#.into()),
        ]);
        let runner = StructuredRunner::new(inner, Some(3));
        let err = runner
            .run(
                &call_with(Some(finding_schema())),
                CancellationToken::new(),
                no_spawn_hook(),
            )
            .await
            .expect_err("the inner failure propagates");
        assert_eq!(err.status(), 424);
        assert_eq!(seen.lock().unwrap().len(), 1, "no second attempt");
    }

    /// A stopped run does not start another attempt.
    #[tokio::test]
    async fn a_stopped_run_does_not_start_an_attempt() {
        let (inner, seen) = scripted(vec![Ok("prose".into()), Ok("prose".into())]);
        let runner = StructuredRunner::new(inner, Some(3));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = runner
            .run(&call_with(Some(finding_schema())), cancel, no_spawn_hook())
            .await
            .expect_err("a stopped run refuses");
        assert_eq!(err.status(), 409);
        assert_eq!(err.to_string(), "workflow stopped");
        assert!(seen.lock().unwrap().is_empty(), "nothing was launched");
    }

    /// An unusable schema is refused BEFORE anything launches.
    #[tokio::test]
    async fn an_unusable_schema_fails_before_the_first_launch() {
        let (inner, seen) = scripted(vec![Ok(r#"{"a":1}"#.into())]);
        let runner = StructuredRunner::new(inner, Some(3));
        let err = runner
            .run(
                &call_with(Some(json!({"type": "object", "properties": {}}))),
                CancellationToken::new(),
                no_spawn_hook(),
            )
            .await
            .expect_err("a schema with no properties is refused");
        assert_eq!(err.status(), 400);
        assert!(seen.lock().unwrap().is_empty(), "nothing billed");
    }
}
