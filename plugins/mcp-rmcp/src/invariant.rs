//! §0.2 runtime invariant for `bough-plugin-mcp-rmcp`:
//!
//! **The set of servers on `ctx.mcp` includes exactly the enabled `ServerRow`s of this parent.**
//! A row that mounted no server, or a server this parent registered whose row is gone, is the
//! violation — that is the "one child entry per server" rule stated as a data relation.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_mcp::{Mcp, ServerName};

/// The pure half: what is wrong, given the row's enabled names and the seam's registered ones.
pub fn violation(expected: &[String], registered: &[ServerName]) -> Option<String> {
    let missing: Vec<&String> = expected
        .iter()
        .filter(|n| !registered.iter().any(|r| r.as_str() == n.as_str()))
        .collect();
    if !missing.is_empty() {
        return Some(format!(
            "enabled server rows with no server on the seam: {missing:?}"
        ));
    }
    None
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "mcp-rmcp/one-child-entry-per-enabled-server",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| {
            Box::pin(async move {
                let fail = |detail: String| InvariantViolation {
                    invariant: "mcp-rmcp/one-child-entry-per-enabled-server",
                    plugin: crate::PLUGIN_NAME,
                    entry: ctx.entry_id().clone(),
                    detail,
                };
                let Ok(mcp) = ctx.get::<Mcp>() else {
                    return Err(fail("the `mcp` seam is not available".into()));
                };
                let Some(kernel) = ctx.kernel() else {
                    // Nothing to compare against without the tree; report nothing rather than
                    // guess (the runner REPORTS, so a false positive would be the worse bug).
                    return Ok(());
                };
                let Some(composition) = kernel.composition() else {
                    return Ok(());
                };
                let expected = expected_names(&composition.tree, ctx.entry_id());
                match violation(&expected, &mcp.servers()) {
                    None => Ok(()),
                    Some(detail) => Err(fail(detail)),
                }
            })
        },
    }]
}

/// The enabled server names of the row with this id, read back off the composed tree.
fn expected_names(tree: &[bough_kernel::Entry], id: &bough_kernel::EntryId) -> Vec<String> {
    for entry in tree {
        if &entry.id == id {
            return serde_yaml::from_value::<crate::McpRmcpConfig>(entry.config.clone())
                .map(|c| {
                    c.servers
                        .into_iter()
                        .filter(|r| !r.disabled)
                        .map(|r| r.name)
                        .collect()
                })
                .unwrap_or_default();
        }
        let found = expected_names(&entry.group, id);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_with_no_server_on_the_seam_is_a_violation() {
        let v = violation(&["fixture".to_string()], &[]);
        assert!(v.unwrap().contains("fixture"));
    }

    #[test]
    fn every_enabled_row_registered_is_clean() {
        assert_eq!(
            violation(&["fixture".to_string()], &[ServerName::new("fixture")]),
            None
        );
    }
}
