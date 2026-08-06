//! Tag normalization + command recording (port of `src/history/record.ts`):
//! normalize/reference grammar, attribution dir-scan caps, one-transaction
//! insert via `Db::record_command`. STUB (wave 2, row 2.9) — except for the
//! three seams below that `hostfn/shell.rs` (row 1.19) imports, ported now
//! because the shell verbs are their consumers: `OUTPUT_HEAD_CHARS`,
//! `spill_path_from`, `normalize_tags`.

/// How much printed output one history row keeps inline.
pub const OUTPUT_HEAD_CHARS: usize = 2_000;

/// The spill file a bounded output points at, parsed back out of the marker
/// (`hostfn/spill.rs`'s `spill_marker`). The marker travels INSIDE the text
/// the program saw, so parsing it here spares the spill module a second
/// return channel it would only ever grow for this one consumer.
pub fn spill_path_from(output: &str) -> Option<String> {
    let re = spill_path_re();
    re.captures(output).map(|c| c[1].to_string())
}

fn spill_path_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"FULL OUTPUT SAVED[^\n]*\n\s+(\S+)\n").unwrap())
}

/// Tags the model may write: short lowercase slugs, colon-separated.
const MAX_TAGS: usize = 8;

/// A REFERENCE: `namespace.id`, pointing at something with an identity outside
/// bough — `linear.eng-1234`, `pr.456`, `commit.3c1c78e`. The dot is the whole
/// rule; dashes and slashes survive INSIDE a reference and nowhere else.
fn is_ref_piece(piece: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^[a-z][a-z0-9]*\.[a-z0-9][a-z0-9._/-]*$").unwrap()
    })
    .is_match(piece)
}

/// Normalize a model-written tag string: lowercase, split into tags, slugify
/// each part, drop empties, cap the count. Returns `""` when nothing survives
/// — which the caller treats as "no tags given".
///
/// Normalization is what makes a folksonomy converge: `PSQL:Migrate` and
/// `psql:migrate` must be the same tag or the popularity stats fragment.
/// Dashes and whitespace are SEPARATORS, not tag characters. A reference
/// (`namespace.id`) is the one exception and passes through whole.
pub fn normalize_tags(raw: Option<&str>) -> String {
    let raw = match raw {
        Some(r) if !r.is_empty() => r,
        _ => return String::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let lower = raw.to_lowercase();
    // Split on colons and whitespace FIRST, so a reference is still whole when
    // it is tested — splitting on dashes up front would have shredded it.
    for piece in lower.split(|c: char| c == ':' || c.is_whitespace()) {
        if piece.is_empty() {
            continue;
        }
        if is_ref_piece(piece) {
            out.push(piece.to_string());
            continue;
        }
        for part in piece.split('-') {
            let tag: String =
                part.chars().filter(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '.')).collect();
            // At least one letter or digit: `...` survives the character
            // filter (dots are legal in a tag) and would then read as a
            // reference, which it is not.
            if tag.chars().any(|c| c.is_ascii_alphanumeric()) {
                out.push(tag);
            }
        }
    }
    out.truncate(MAX_TAGS);
    out.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_splits_and_slugifies() {
        assert_eq!(normalize_tags(Some(" Git : PUSH ")), "git:push");
        assert_eq!(normalize_tags(Some("Repo: Inspect")), "repo:inspect");
        assert_eq!(normalize_tags(Some("repo-inspect")), "repo:inspect");
        assert_eq!(normalize_tags(Some(":::")), "");
        assert_eq!(normalize_tags(None), "");
        assert_eq!(normalize_tags(Some("")), "");
    }

    #[test]
    fn references_survive_whole() {
        assert_eq!(normalize_tags(Some("linear.eng-1234")), "linear.eng-1234");
        assert_eq!(normalize_tags(Some("branch.claude/tags-history")), "branch.claude/tags-history");
        assert_eq!(normalize_tags(Some("ENG-1234")), "eng:1234");
    }

    #[test]
    fn spill_path_is_parsed_back_out_of_the_marker() {
        let marker = "head\n[… 1 chars omitted from the middle. FULL OUTPUT SAVED — 30,000 chars:\n   /tmp/s/bash-001.log\n   rg -n 'error|fail' '/tmp/s/bash-001.log'\n…]\ntail";
        assert_eq!(spill_path_from(marker).as_deref(), Some("/tmp/s/bash-001.log"));
        assert_eq!(spill_path_from("plain output"), None);
    }
}
