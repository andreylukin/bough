//! Invariant (§7): the ordering is intent row → outward act → `action/done`, and the marker the
//! journal derived is embedded IN the artifact, so reconciliation is a lookup against the world
//! rather than a guess. The two configured delays are the two windows a `kill -9` lands in.

use std::sync::Arc;
use std::time::Duration;

use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, ExecuteRequest,
};

use crate::ShimConfig;

/// The Provider. Registers through `ActionsHandle::provider`, like any Phase-6 row will.
pub struct GhShimProvider {
    pub cfg: Arc<ShimConfig>,
}

impl GhShimProvider {
    /// The Provider over a row's config.
    pub fn new(cfg: Arc<ShimConfig>) -> GhShimProvider {
        GhShimProvider { cfg }
    }
}

#[async_trait::async_trait]
impl ActionProvider for GhShimProvider {
    fn kinds(&self) -> Vec<ActionKind> {
        self.cfg.kinds.clone()
    }

    /// Embeds `req.marker` in the artifact (PR body / commit trailer / comment suffix) exactly as
    /// §7 requires.
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let kind = req.request.kind;
        let fail = |source: anyhow::Error| ActionError::Provider {
            kind: kind.as_str(),
            source,
        };

        if self.cfg.delay_before_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.cfg.delay_before_ms)).await;
        }

        let args = argv(&self.cfg, kind, &req.canonical_target, &req.marker);
        // Counted at the moment of the act, not after it: a kill between the call and the count
        // must not be able to hide an invocation from the invariant.
        crate::invariant::record(&req.idem_key);
        let out = tokio::process::Command::new(&args[0])
            .args(&args[1..])
            .output()
            .await
            .map_err(|e| fail(anyhow::Error::new(e).context(format!("running `{}`", args[0]))))?;

        if !out.status.success() {
            let code = out.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(fail(anyhow::anyhow!(
                "`{}` exited {code}: {stderr}",
                args[0]
            )));
        }

        if self.cfg.delay_after_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.cfg.delay_after_ms)).await;
        }

        let locator = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(ActionArtifact {
            locator,
            marker: req.marker.clone(),
            detail: serde_json::json!({ "argv": args }),
        })
    }
}

/// PURE: the argv this Provider runs for a kind and a canonical target. The binary name comes from
/// config and is NEVER hardcoded, so a test's recording shim is reachable without a PATH trick in
/// the crate itself.
///
/// The marker rides in the field the world will show back on a lookup: a PR body, a commit
/// trailer, a comment suffix.
pub fn argv(
    cfg: &ShimConfig,
    kind: ActionKind,
    canonical_target: &str,
    marker: &str,
) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match kind {
        ActionKind::OpenPr => vec![
            s(&cfg.gh),
            s("pr"),
            s("create"),
            s("--repo"),
            s(canonical_target),
            s("--body"),
            marker.to_string(),
        ],
        ActionKind::PushToPr => vec![
            s(&cfg.gh),
            s("pr"),
            s("push"),
            s("--repo"),
            s(canonical_target),
            s("--trailer"),
            format!("Bough-Action: {marker}"),
        ],
        ActionKind::BotThreadOp => vec![
            s(&cfg.gh),
            s("pr"),
            s("comment"),
            s("--repo"),
            s(canonical_target),
            s("--body"),
            marker.to_string(),
        ],
        ActionKind::LinearWrite => vec![
            s(&cfg.gh),
            s("issue"),
            s("comment"),
            s("--repo"),
            s(canonical_target),
            s("--body"),
            marker.to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ShimConfig {
        ShimConfig {
            gh: "gh-shim".into(),
            kinds: ActionKind::all().to_vec(),
            delay_before_ms: 0,
            delay_after_ms: 0,
        }
    }

    #[test]
    fn the_marker_is_embedded_in_every_artifact() {
        let marker = bough_plugin_actions::marker_for(&bough_plugin_ledger::IdemKey::new(
            "0123456789abcdef0123456789abcdef",
        ));
        for kind in ActionKind::all() {
            let args = argv(&cfg(), *kind, "owner/repo", &marker);
            assert!(
                args.iter().any(|a| a.contains(&marker)),
                "`{}` must carry the marker into the artifact; got {args:?}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn the_four_kinds_are_exactly_the_sanctioned_ones() {
        // §7's set is CLOSED, and this Provider's default claim is exactly it — no fifth kind and
        // no kind of its own invention.
        assert_eq!(ActionKind::all().len(), 4);
        assert_eq!(
            GhShimProvider::new(Arc::new(cfg())).kinds(),
            ActionKind::all().to_vec()
        );
    }

    #[test]
    fn an_unregistered_kind_is_refused() {
        // A Provider that claims one kind does not quietly serve the others: `ActionsHandle`
        // asks `kinds()` and answers `NoProvider` for everything outside it.
        let mut c = cfg();
        c.kinds = vec![ActionKind::OpenPr];
        let p = GhShimProvider::new(Arc::new(c));
        assert_eq!(p.kinds(), vec![ActionKind::OpenPr]);
        for kind in [
            ActionKind::PushToPr,
            ActionKind::BotThreadOp,
            ActionKind::LinearWrite,
        ] {
            assert!(
                !p.kinds().contains(&kind),
                "`{}` is not claimed",
                kind.as_str()
            );
        }
    }

    #[test]
    fn the_shim_binary_name_comes_from_config_and_is_never_hardcoded() {
        let mut c = cfg();
        c.gh = "/tmp/recording-shim".into();
        for kind in ActionKind::all() {
            let args = argv(&c, *kind, "owner/repo", "m");
            assert_eq!(args[0], "/tmp/recording-shim");
            // And nothing downstream reintroduces the real one.
            assert!(!args[1..].iter().any(|a| a == "gh"), "got {args:?}");
        }
    }

    #[test]
    fn every_kind_carries_the_marker_and_the_configured_binary() {
        for kind in ActionKind::all() {
            let args = argv(&cfg(), *kind, "owner/repo", "bough-action:abc");
            assert_eq!(args[0], "gh-shim", "the binary is never hardcoded");
            assert!(
                args.iter().any(|a| a.contains("bough-action:abc")),
                "{} embeds the marker; got {args:?}",
                kind.as_str()
            );
            assert!(
                args.iter().any(|a| a == "owner/repo"),
                "{} acts on the canonical target; got {args:?}",
                kind.as_str()
            );
        }
    }
}
