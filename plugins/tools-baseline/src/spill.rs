//! Invariant (§9's named example): oversized tool output is SPILLED to a file and replaced inline
//! by a locator, so the model always sees a bounded result and never a truncated one that
//! pretends to be whole.

use std::path::PathBuf;

use bough_plugin_tools::PostExecute;

/// Where a spilled result lands. Not config: the row's `root` is the TASK tree, and a spill file
/// is harness bookkeeping that must not appear in it (§7's containment check exists to keep the
/// task tree clean).
pub fn spill_dir() -> PathBuf {
    std::env::temp_dir().join("bough-spill")
}

/// The `tools/post-execute` listener the row registers.
///
/// A spill replaces the CONTENT (never the value: `accept` may replace one or the other, §9) with
/// a head plus a locator naming the file that holds the whole output.
pub fn spill_if_oversized(max_output_bytes: usize, post: &mut PostExecute) {
    let result = post.result();
    let full = result.content.clone();
    if full.len() <= max_output_bytes {
        return;
    }
    let dir = spill_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!(
        "{}-{}.txt",
        result.call,
        uuid_ish(&full, result.name.as_str())
    ));
    if std::fs::write(&path, full.as_bytes()).is_err() {
        return;
    }
    // Cut on a char boundary: the head is shown verbatim and must stay valid UTF-8.
    let mut head_end = max_output_bytes;
    while head_end > 0 && !full.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let head = &full[..head_end];
    post.accept_content(format!(
        "{head}\n[output spilled: {} bytes total, first {} shown; full output at {}]",
        full.len(),
        head_end,
        path.display()
    ));
}

/// A short stable suffix, so two spills of one call do not collide and the name carries no clock.
fn uuid_ish(content: &str, name: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    name.hash(&mut h);
    format!("{:x}", h.finish())
}
