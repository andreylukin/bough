//! §0.2 runtime invariant for `bough-plugin-mcp`:
//!
//! **Every `McpCallResult` the seam handed back carries EXACTLY the cite `cite_of` mints for its
//! (server, tool, args), and no cite a server supplied.** The seam mints the citation; a foreign
//! server never does. The check is a fold over the observed call stream, bounded.

use std::sync::OnceLock;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

/// How many observations are kept. Bounded on purpose: an invariant that grows without limit is
/// a leak, not a check.
const CAP: usize = 4096;

/// One observed call, reduced to what the invariant is about.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    /// What `cite_of` minted for this call.
    pub minted: String,
    /// What the client's own result carried, BEFORE the seam overwrote it.
    pub client_supplied: Vec<String>,
    /// What the caller actually received.
    pub delivered: Vec<String>,
}

fn stream() -> &'static Mutex<Vec<Obs>> {
    static S: OnceLock<Mutex<Vec<Obs>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one call.
pub fn record(obs: Obs) {
    let mut s = stream().lock();
    s.push(obs);
    let len = s.len();
    if len > CAP {
        s.drain(0..len - CAP);
    }
}

/// Forget everything recorded. Tests use it; the seam has no per-fiber stream to forget.
pub fn reset() {
    stream().lock().clear();
}

/// The pure half: the first violation in `obs`, if any.
pub fn violation(obs: &[Obs]) -> Option<String> {
    for o in obs {
        if o.delivered != vec![o.minted.clone()] {
            return Some(format!(
                "a call delivered cites {:?}, not exactly the minted `{}`",
                o.delivered, o.minted
            ));
        }
        for supplied in &o.client_supplied {
            if supplied != &o.minted && o.delivered.contains(supplied) {
                return Some(format!(
                    "a server-supplied cite `{supplied}` survived into the delivered result"
                ));
            }
        }
    }
    None
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "mcp/cites-are-minted-by-the-seam",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| {
            Box::pin(async move {
                let obs = stream().lock().clone();
                match violation(&obs) {
                    None => Ok(()),
                    Some(detail) => Err(InvariantViolation {
                        invariant: "mcp/cites-are-minted-by-the-seam",
                        plugin: crate::PLUGIN_NAME,
                        entry: ctx.entry_id().clone(),
                        detail,
                    }),
                }
            })
        },
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_cite_alone_is_clean() {
        assert_eq!(
            violation(&[Obs {
                minted: "mcp:s:t:abc".into(),
                client_supplied: vec!["gh:o/r#1".into()],
                delivered: vec!["mcp:s:t:abc".into()],
            }]),
            None
        );
    }

    #[test]
    fn a_server_supplied_cite_that_survived_is_a_violation() {
        let v = violation(&[Obs {
            minted: "mcp:s:t:abc".into(),
            client_supplied: vec!["gh:o/r#1".into()],
            delivered: vec!["mcp:s:t:abc".into(), "gh:o/r#1".into()],
        }]);
        assert!(v.unwrap().contains("not exactly the minted"));
    }
}
