//! `callKey` — the journal replay key (port of the hashing half of
//! `src/workflow/run.ts`).
//!
//! FNV-1a over the canonical call shape, twice with different offsets so an
//! accidental 32-bit collision would have to happen twice; a collision here
//! silently returns another agent's report.
//!
//! **Port bit-for-bit or old journals stop replaying** (spec §4). Three
//! hazards, all of which this module exists to hold:
//!
//!   1. the hash walks **UTF-16 code units** (`str::encode_utf16`), never
//!      bytes and never `char`s, and multiplies with `wrapping_mul`
//!      (= `Math.imul`);
//!   2. the hashed string is `JSON.stringify` of a five-element array, so the
//!      serializer has to agree with JS about number formatting (`1.0` prints
//!      as `1`) and string escaping;
//!   3. both 32-bit halves are ZERO-PADDED to 8 hex digits — without it the
//!      boundary between them floats and ~12% of keys collide.
//!
//! The hashed `label` is the DETERMINISTIC first-line default (or the explicit
//! label), never the sibling-aware display label, so replay never depends on
//! which siblings happened to exist.

use serde_json::Value;

use super::runner::AgentCall;

/// Truncate to `n` UTF-16 code units and mark it, exactly as the TS `clip()`
/// does. Cosmetic in notes, load-bearing in keys: the default label is a
/// clipped first line, and the clip therefore feeds [`call_key`].
pub fn clip(s: &str, n: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    if units.len() <= n {
        return s.to_string();
    }
    // A clip that lands mid-surrogate-pair yields the replacement char here
    // where JS yields a lone surrogate; the pair is one grapheme either way and
    // both are stable, so a key stays reproducible run to run.
    format!(
        "{}…",
        String::from_utf16_lossy(&units[..n.saturating_sub(1)])
    )
}

/// The 16-hex-character content hash of one call: everything that decides what
/// the subagent will be asked.
///
/// `effective_model` is the model a call that names none will actually run on
/// (session pin, else ctx default, else the built-in). Hashing the RESOLVED
/// model is the fix for a real bug: repinning a session and rerunning a
/// byte-identical script replayed every row from cache and handed back the OLD
/// model's answers as a fresh run on the new one.
pub fn call_key(call: &AgentCall, effective_model: Option<&str>) -> String {
    let model = call.model.as_deref().or(effective_model).unwrap_or("");
    let shape = Value::Array(vec![
        Value::String(call.prompt.clone()),
        Value::String(call.label.clone()),
        Value::String(call.phase.clone().unwrap_or_default()),
        Value::String(model.to_string()),
        // Canonicalized: `JSON.stringify` preserves insertion order, so a
        // reordered or prettier-formatted schema literal hashed differently and
        // re-ran every call that used it.
        canonical_json(call.schema.as_ref().unwrap_or(&Value::Null)),
    ]);
    fnv_pair(&js_stringify(&shape))
}

/// The two FNV-1a-style passes, over UTF-16 code units, zero-padded.
fn fnv_pair(s: &str) -> String {
    let mut a: u32 = 0x811c_9dc5;
    let mut b: u32 = 0x0100_0193;
    for c in s.encode_utf16() {
        let c = c as u32;
        a = (a ^ c).wrapping_mul(0x0100_0193);
        b = (b ^ ((c + 7) & 0xffff)).wrapping_mul(0x0100_0193);
    }
    format!("{a:08x}{b:08x}")
}

/// Order-independent JSON for hashing: objects get their keys sorted,
/// recursively. Arrays keep their order — position is meaning there.
pub fn canonical_json(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical_json(&map[k]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `JSON.stringify`, faithfully enough to hash with.
///
/// `serde_json::to_string` agrees with JS on strings (same escapes, same
/// lowercase `\u00xx`, non-ASCII left alone) and on integers, but not on
/// integral floats: a schema carrying `1.0` prints as `1` in JS and `1.0`
/// through serde. Same schema, different key, every call re-run — so the
/// number arm is written out here.
fn js_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number(n),
        Value::String(_) => serde_json::to_string(v).expect("string is always serializable"),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(js_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).expect("key is always serializable"),
                        js_stringify(val)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

fn js_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if !f.is_finite() {
        return "null".to_string(); // JSON.stringify(NaN|Infinity) === "null"
    }
    // JS prints an integral double without a fractional part, up to the point
    // where it switches to exponent notation.
    if f.fract() == 0.0 && f.abs() < 1e21 {
        return format!("{}", f as i128);
    }
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(prompt: &str, label: &str) -> AgentCall {
        AgentCall {
            prompt: prompt.to_string(),
            label: label.to_string(),
            phase: None,
            model: None,
            schema: None,
        }
    }

    /// The row-3.7 gate. These digests come from the TS `callKey` (run.ts) run
    /// on the same inputs — the whole point of the algorithm is that a journal
    /// written by one engine replays under the other, so the expected values
    /// are cross-engine constants, not whatever this implementation produces.
    #[test]
    fn call_key_matches_the_ts_digests() {
        assert_eq!(
            call_key(&call("audit the handlers", "audit"), None),
            "10ea2db8c2987079"
        );
        assert_eq!(
            call_key(&call("audit the handlers", "audit"), Some("gpt-5.6")),
            "667a9eb5b0d994d3"
        );
        // Every hashed component moves the key.
        assert_ne!(
            call_key(&call("audit the handlers", "audit"), None),
            call_key(&call("audit the handlers!", "audit"), None)
        );
        assert_ne!(
            call_key(&call("audit the handlers", "audit"), None),
            call_key(&call("audit the handlers", "audit2"), None)
        );
    }

    /// Non-BMP text is where a bytes-or-chars loop diverges from JS: "🐛" is
    /// ONE char, FOUR bytes and TWO UTF-16 code units, and only the last is
    /// what the TS hash walked.
    #[test]
    fn call_key_hashes_utf16_code_units_not_bytes_or_chars() {
        assert_eq!(
            call_key(&call("fix the 🐛 in parse()", "bug"), None),
            "aae9507962b08e6d"
        );
        // A pure sanity check on the encoding assumption itself.
        assert_eq!("🐛".encode_utf16().count(), 2);
        assert_eq!("🐛".chars().count(), 1);
        assert_eq!("🐛".len(), 4);
    }

    /// Both halves are 8 hex digits, always. Without the padding the boundary
    /// floats and (0x1, 0x23) and (0x12, 0x3) both encode "123".
    #[test]
    fn both_halves_are_zero_padded_to_eight() {
        for i in 0..200 {
            let k = call_key(&call(&format!("prompt {i}"), "l"), None);
            assert_eq!(k.len(), 16, "{k}");
            assert!(k.chars().all(|c| c.is_ascii_hexdigit()), "{k}");
        }
    }

    /// A reordered or reformatted schema literal is the SAME call.
    #[test]
    fn a_reordered_schema_hashes_the_same_and_a_changed_one_does_not() {
        let mut a = call("x", "l");
        a.schema = Some(json!({
            "type": "object",
            "properties": {"b": {"type": "string"}, "a": {"type": "integer"}},
            "additionalProperties": false
        }));
        let mut b = call("x", "l");
        b.schema = Some(json!({
            "additionalProperties": false,
            "properties": {"a": {"type": "integer"}, "b": {"type": "string"}},
            "type": "object"
        }));
        // Both sides, and the TS digest for the same pair.
        assert_eq!(call_key(&a, None), call_key(&b, None));
        assert_eq!(call_key(&a, None), "87fdcc506410d75e");

        // A schema carrying an integral float: `1.0` is `1` to JSON.stringify,
        // and a serde-default `1.0` would hash a different call forever.
        let mut f = call("x", "l");
        f.schema = Some(json!({
            "type": "object",
            "properties": {"n": {"type": "number", "default": 1.0}},
            "additionalProperties": false
        }));
        assert_eq!(call_key(&f, None), "570a363df4df85b4");

        let mut c = call("x", "l");
        c.schema = Some(json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        }));
        assert_ne!(call_key(&a, None), call_key(&c, None));
        // And a schema at all is different from no schema.
        assert_ne!(call_key(&a, None), call_key(&call("x", "l"), None));
    }

    /// An explicit `model` outranks the resolved one; the resolved one is
    /// hashed when the script named none (the repinning bug).
    #[test]
    fn the_resolved_model_is_hashed_when_the_call_names_none() {
        let mut named = call("x", "l");
        named.model = Some("claude-opus".into());
        assert_eq!(
            call_key(&named, Some("gpt-5.6")),
            call_key(&named, Some("anything-else"))
        );
        assert_ne!(
            call_key(&call("x", "l"), Some("a")),
            call_key(&call("x", "l"), Some("b"))
        );
    }

    /// `js_stringify` must agree with `JSON.stringify` on the shapes a schema
    /// can carry, or the same schema hashes two ways.
    #[test]
    fn integral_floats_print_the_js_way() {
        assert_eq!(js_stringify(&json!(1.0)), "1");
        assert_eq!(js_stringify(&json!(-3.0)), "-3");
        assert_eq!(js_stringify(&json!(1.5)), "1.5");
        assert_eq!(js_stringify(&json!(7)), "7");
        assert_eq!(js_stringify(&json!("a\"b\nc")), r#""a\"b\nc""#);
        assert_eq!(
            js_stringify(&json!({"b": 1, "a": [true, null]})),
            r#"{"a":[true,null],"b":1}"#
        );
    }

    #[test]
    fn clip_counts_utf16_units_and_marks_the_cut() {
        assert_eq!(clip("short", 40), "short");
        assert_eq!(clip(&"x".repeat(41), 40), format!("{}…", "x".repeat(39)));
        assert_eq!(clip("exactly ten", 11), "exactly ten");
    }
}
