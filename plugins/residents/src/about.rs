//! Invariant: an about-line is ONE clean sentence (phase ux1 §2.10, minor 29). It is the most
//! repeated text on the screen, so it carries no markdown markers, no spliced fragments and no
//! dangling emphasis: `read mail \`say hi\`; Hi; ! 👋 ; **` was three fragments and a broken bold.

/// PURE: one clean sentence. Markdown stripped, emoji kept, whitespace collapsed, clipped on a
/// WORD boundary with `…`, never spliced with `;`.
pub fn one_sentence(raw: &str, max_chars: usize) -> String {
    let _ = (raw, max_chars);
    todo!("WP-7")
}
