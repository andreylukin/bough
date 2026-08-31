//! Invariant (§7, P2-D22): there are FOUR outward acts and the set is CLOSED. A kind not in this
//! enum cannot be spelled at all, and a kind no Provider registered does not exist as far as the
//! executor is concerned — "Slack send is not a kind" is a compile-time fact, not a lookup.
//!
//! Canonicalisation is the other half of the same invariant: the idem key hashes the CANONICAL
//! target, so two spellings of one target collide in the journal instead of acting twice (§7).

use crate::error::ActionError;

/// §7's four sanctioned outward acts.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    OpenPr,
    PushToPr,
    BotThreadOp,
    LinearWrite,
}

impl ActionKind {
    /// The spelling used in the journal, in error messages and in the idem key.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::OpenPr => "open_pr",
            ActionKind::PushToPr => "push_to_pr",
            ActionKind::BotThreadOp => "bot_thread_op",
            ActionKind::LinearWrite => "linear_write",
        }
    }

    /// The kind a WIRE SPELLING names, or `None`. The set is closed, so this is the ONE place a
    /// string becomes a kind (merge note 3).
    pub fn parse(name: &str) -> Option<ActionKind> {
        ActionKind::all()
            .iter()
            .copied()
            .find(|k| k.as_str() == name)
    }

    /// The four names, comma-separated, for a refusal that has to say what the vocabulary IS.
    pub const KNOWN: &'static str = "open_pr, push_to_pr, bot_thread_op, linear_write";

    /// Every kind, for `--dump-config` and for the tool row's registrations.
    pub fn all() -> &'static [ActionKind] {
        &[
            ActionKind::OpenPr,
            ActionKind::PushToPr,
            ActionKind::BotThreadOp,
            ActionKind::LinearWrite,
        ]
    }
}

/// What an action acts on, as the caller spelled it.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionTarget {
    pub raw: String,
}

impl ActionTarget {
    /// A target from anything string-shaped.
    pub fn new(raw: impl Into<String>) -> ActionTarget {
        ActionTarget { raw: raw.into() }
    }

    /// Canonical form per kind (lowercased host, no trailing slash, `owner/repo#number`,
    /// `TEAM-123`). The idem key hashes THIS, so two spellings of one target collide (§7).
    pub fn canonical(&self, kind: ActionKind) -> Result<String, ActionError> {
        let raw = self.raw.trim();
        let bad = || ActionError::BadTarget(self.raw.clone(), kind.as_str());
        match kind {
            ActionKind::OpenPr => {
                let gh = parse_github(raw).ok_or_else(bad)?;
                // A PR is opened ON A REPO. A target that names one pull request is a different
                // thing and is refused rather than silently widened.
                if gh.number.is_some() {
                    return Err(bad());
                }
                Ok(format!("{}/{}", gh.owner, gh.repo))
            }
            ActionKind::PushToPr | ActionKind::BotThreadOp => {
                let gh = parse_github(raw).ok_or_else(bad)?;
                let n = gh.number.ok_or_else(bad)?;
                Ok(format!("{}/{}#{n}", gh.owner, gh.repo))
            }
            ActionKind::LinearWrite => canonical_linear(raw).ok_or_else(bad),
        }
    }
}

/// A GitHub target, taken apart.
struct GhTarget {
    owner: String,
    repo: String,
    number: Option<u64>,
}

/// `owner/repo`, `owner/repo#12`, `https://GitHub.com/Owner/Repo/pull/12/files`, … → one shape.
///
/// Pure, and the ONLY place a GitHub target is read: `push_to_pr` on a PR url and `push_to_pr` on
/// `owner/repo#12` must land on the same string or the journal cannot collide them.
fn parse_github(raw: &str) -> Option<GhTarget> {
    let mut s = raw.trim();
    if let Some((_scheme, rest)) = s.split_once("://") {
        let (host, path) = rest.split_once('/')?;
        let host = host.to_ascii_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host);
        if host != "github.com" {
            return None;
        }
        s = path;
    }
    let s = s.trim_matches('/');
    // `owner/repo#12` — the shorthand.
    let (path, hash_number) = match s.split_once('#') {
        Some((p, n)) => (p, Some(n.parse::<u64>().ok()?)),
        None => (s, None),
    };
    let mut segs = path.split('/').filter(|x| !x.is_empty());
    let owner = segs.next()?.to_ascii_lowercase();
    let repo = segs.next()?.to_ascii_lowercase();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    // `/pull/12`, `/pulls/12`, `/issues/12` — the url forms.
    let mut number = hash_number;
    if let Some(kind) = segs.next() {
        if !matches!(kind, "pull" | "pulls" | "issues") {
            return None;
        }
        let n = segs.next()?.parse::<u64>().ok()?;
        if number.is_some_and(|h| h != n) {
            return None;
        }
        number = Some(n);
    }
    Some(GhTarget {
        owner,
        repo,
        number,
    })
}

/// `TEAM-123` out of `team-123` or `https://linear.app/acme/issue/TEAM-123/some-slug`.
fn canonical_linear(raw: &str) -> Option<String> {
    let s = raw.trim().trim_matches('/');
    let s = match s.split_once("://") {
        Some((_, rest)) => rest,
        None => s,
    };
    // The identifier is whichever segment looks like one; a url carries slug segments too.
    s.split('/')
        .filter_map(|seg| {
            let (team, num) = seg.split_once('-')?;
            if team.is_empty()
                || !team.chars().all(|c| c.is_ascii_alphanumeric())
                || !team.chars().next()?.is_ascii_alphabetic()
                || num.is_empty()
                || !num.chars().all(|c| c.is_ascii_digit())
            {
                return None;
            }
            Some(format!("{}-{}", team.to_ascii_uppercase(), num))
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_url_and_the_shorthand_are_one_open_pr_target() {
        let a = ActionTarget::new("https://GitHub.com/Owner/Repo/");
        let b = ActionTarget::new("owner/repo");
        assert_eq!(
            a.canonical(ActionKind::OpenPr).unwrap(),
            b.canonical(ActionKind::OpenPr).unwrap()
        );
    }

    #[test]
    fn a_pull_request_url_and_the_shorthand_are_one_push_target() {
        let a = ActionTarget::new("https://github.com/Owner/Repo/pull/12/files");
        let b = ActionTarget::new("owner/repo#12");
        assert_eq!(a.canonical(ActionKind::PushToPr).unwrap(), "owner/repo#12");
        assert_eq!(b.canonical(ActionKind::PushToPr).unwrap(), "owner/repo#12");
    }

    #[test]
    fn a_push_target_without_a_number_is_refused() {
        let e = ActionTarget::new("owner/repo")
            .canonical(ActionKind::PushToPr)
            .unwrap_err();
        assert!(matches!(e, ActionError::BadTarget(_, "push_to_pr")));
    }

    #[test]
    fn a_non_github_host_is_refused() {
        assert!(ActionTarget::new("https://gitlab.com/o/r")
            .canonical(ActionKind::OpenPr)
            .is_err());
    }

    #[test]
    fn a_linear_url_and_the_bare_identifier_are_one_target() {
        let a = ActionTarget::new("https://linear.app/acme/issue/team-123/fix-the-thing");
        let b = ActionTarget::new("TEAM-123");
        assert_eq!(a.canonical(ActionKind::LinearWrite).unwrap(), "TEAM-123");
        assert_eq!(b.canonical(ActionKind::LinearWrite).unwrap(), "TEAM-123");
    }

    #[test]
    fn a_target_that_names_nothing_is_refused_rather_than_passed_through() {
        assert!(ActionTarget::new("just some words")
            .canonical(ActionKind::LinearWrite)
            .is_err());
        assert!(ActionTarget::new("owner")
            .canonical(ActionKind::OpenPr)
            .is_err());
    }
}
