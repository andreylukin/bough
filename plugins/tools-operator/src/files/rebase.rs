//! Invariant: a rebase re-checks the ACTUAL lines rather than trusting a tag. An untouched range
//! moves; a touched one conflicts, naming the line range, rather than being applied blind.
//!
//! A port of main's `line_map` / `rebase_ops`.

use super::grammar::{OpKind, PatchError, PatchOp};

/// Past this many lines of divergence the LCS is skipped and every line in the diverged middle is
/// reported as changed. Two agents editing one file almost always diverge in a single small
/// region; the cap keeps a pathological diff from becoming an O(n·m) stall. Exceeding it costs a
/// rejected patch, never a wrong one.
const LCS_CAP: usize = 400;

/// A map from each line of `base` to its line in `cur`, or `None` where the line was changed or
/// deleted by whoever wrote the file in the meantime.
///
/// Common prefix and suffix are trimmed first — that is what makes this cheap — and an LCS over
/// the diverged middles supplies the rest. The result is monotonically increasing.
pub fn line_map(base: &[String], cur: &[String]) -> Vec<Option<usize>> {
    let mut map: Vec<Option<usize>> = vec![None; base.len()];
    if base.is_empty() {
        return map;
    }
    let mut p = 0;
    while p < base.len() && p < cur.len() && base[p] == cur[p] {
        map[p] = Some(p);
        p += 1;
    }
    let mut s = 0;
    while s < base.len() - p
        && s < cur.len().saturating_sub(p)
        && base[base.len() - 1 - s] == cur[cur.len() - 1 - s]
    {
        map[base.len() - 1 - s] = Some(cur.len() - 1 - s);
        s += 1;
    }
    let bm = &base[p..base.len() - s];
    let cm = &cur[p..cur.len() - s];
    if bm.is_empty() || cm.is_empty() {
        return map;
    }
    if bm.len() > LCS_CAP || cm.len() > LCS_CAP {
        return map;
    }

    let (n, m) = (bm.len(), cm.len());
    let mut dp = vec![0usize; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[at(i, j)] = if bm[i] == cm[j] {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if bm[i] == cm[j] {
            map[p + i] = Some(p + j);
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    map
}

/// What rebasing a file's operations onto a moved file produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebaseResult {
    /// The file did not move: the ops stand as written.
    Unchanged,
    /// The operations were shifted onto the current coordinates.
    Rebased(Vec<PatchOp>),
    Conflict(RebaseConflict),
}

/// The conflict a rebase reports, in the coordinates the model wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseConflict {
    pub path: String,
    pub from: usize,
    pub to: usize,
    pub detail: String,
}

impl From<RebaseConflict> for PatchError {
    fn from(c: RebaseConflict) -> PatchError {
        PatchError::Conflict {
            path: c.path,
            from: c.from,
            to: c.to,
            detail: c.detail,
        }
    }
}

/// Move `ops` from the coordinates of `base` (the text the agent read) onto `cur` (the text as it
/// stands now).
///
/// An op survives when every line it names is still present AND still contiguous AND still in the
/// same relative order. Checking only the endpoints would accept an op whose interior was
/// rewritten in place — the silent lost update this module exists to prevent.
pub fn rebase_ops(ops: &[PatchOp], base: &[String], cur: &[String]) -> RebaseResult {
    if base == cur {
        return RebaseResult::Unchanged;
    }
    let map = line_map(base, cur);
    let mut out: Vec<PatchOp> = Vec::new();
    for op in ops {
        if op.kind == OpKind::InsHead || op.kind == OpKind::InsTail {
            out.push(op.clone());
            continue;
        }
        let op_a = op.a.expect("line-anchored op has a");
        let op_b = op.b.expect("line-anchored op has b");
        let conflict = |detail: &str| {
            RebaseResult::Conflict(RebaseConflict {
                path: op.path.clone(),
                from: op_a,
                to: op_b,
                detail: detail.to_string(),
            })
        };
        let a = map.get(op_a - 1).copied().flatten();
        let b = map.get(op_b - 1).copied().flatten();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            _ => return conflict("they were rewritten"),
        };
        if b - a != op_b - op_a {
            return conflict("lines were inserted inside them");
        }
        // EVERY line in the span, not just its endpoints.
        for k in (op_a - 1)..=(op_b - 1) {
            if map.get(k).copied().flatten() != Some(a + (k - (op_a - 1))) {
                return conflict("they were rewritten");
            }
        }
        out.push(PatchOp {
            a: Some(a + 1),
            b: Some(b + 1),
            ..op.clone()
        });
    }
    RebaseResult::Rebased(out)
}
