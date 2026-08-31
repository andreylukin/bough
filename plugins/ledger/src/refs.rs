//! Invariant: `step_refs` is DERIVED, never caller-supplied, and derived by THIS function in every
//! provider — which is why the two providers' matching indexes cannot diverge (§3).

use std::collections::BTreeSet;

use crate::id::Ref;
use crate::step::Cite;

/// Every `ref` / `refs` value found at ANY depth of `body` (a string, or an array of strings).
/// Deterministic, order-independent, allocation-bounded. Non-string values are ignored.
pub fn body_refs(body: &serde_json::Value) -> BTreeSet<Ref> {
    let mut out = BTreeSet::new();
    walk(body, &mut out);
    out
}

/// One value. Depth is bounded by the body's own nesting, which the schema validator has already
/// accepted, so no explicit depth guard is needed here.
fn walk(v: &serde_json::Value, out: &mut BTreeSet<Ref>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                if k == "ref" || k == "refs" {
                    collect(child, out);
                }
                walk(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, out);
            }
        }
        _ => {}
    }
}

/// The value of a `ref` / `refs` key: a string, or an array of strings. Anything else is IGNORED —
/// a number or an object under `ref` is not a ref, and inventing one would put a value into the
/// canonical matching index that no router could ever have meant.
fn collect(v: &serde_json::Value, out: &mut BTreeSet<Ref>) {
    match v {
        serde_json::Value::String(s) => {
            out.insert(Ref::new(s));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let serde_json::Value::String(s) = item {
                    out.insert(Ref::new(s));
                }
            }
        }
        _ => {}
    }
}

/// The union of every cite's `ref` and [`body_refs`]. This is a step's canonical ref set.
pub fn derive_step_refs(cites: &[Cite], body: &serde_json::Value) -> BTreeSet<Ref> {
    let mut out = body_refs(body);
    for c in cites {
        out.insert(c.r#ref.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cite(r: &str) -> Cite {
        Cite {
            r#ref: Ref::new(r),
            url: None,
        }
    }

    fn set(items: &[&str]) -> BTreeSet<Ref> {
        items.iter().map(Ref::new).collect()
    }

    #[test]
    fn cites_become_refs() {
        let refs = derive_step_refs(&[cite("gh:o/r#12"), cite("step:abc")], &json!({}));
        assert_eq!(refs, set(&["gh:o/r#12", "step:abc"]));
    }

    #[test]
    fn body_ref_key_at_any_depth() {
        let body = json!({
            "outer": { "middle": [ { "ref": "gh:o/r#7" } ] },
            "ref": "linear:ENG-1"
        });
        assert_eq!(body_refs(&body), set(&["gh:o/r#7", "linear:ENG-1"]));
    }

    #[test]
    fn body_refs_array_form() {
        let body = json!({ "refs": ["a:1", "b:2"], "nested": { "refs": ["c:3"] } });
        assert_eq!(body_refs(&body), set(&["a:1", "b:2", "c:3"]));
    }

    #[test]
    fn extraction_is_order_independent() {
        // The same refs, written in a different key order and a different array order.
        let one = json!({ "a": { "ref": "x:1" }, "b": { "refs": ["y:2", "z:3"] } });
        let two = json!({ "b": { "refs": ["z:3", "y:2"] }, "a": { "ref": "x:1" } });
        assert_eq!(body_refs(&one), body_refs(&two));
        assert_eq!(
            derive_step_refs(&[cite("q:9"), cite("p:8")], &one),
            derive_step_refs(&[cite("p:8"), cite("q:9")], &two)
        );
    }

    #[test]
    fn a_non_string_ref_value_is_ignored() {
        let body =
            json!({ "ref": 12, "refs": [3, "ok:1", { "ref": "deep:2" }], "o": { "ref": null } });
        // 12, 3 and the object are not refs; the string inside the array is, and so is the `ref`
        // found by the ordinary recursion into that object.
        assert_eq!(body_refs(&body), set(&["ok:1", "deep:2"]));
    }
}
