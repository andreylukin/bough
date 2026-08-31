//! Invariant: dedup is ORDER-FAITHFUL and never invents text. Files are processed in config
//! order; a block is kept the FIRST time its content appears and dropped on every later
//! appearance (exact after normalization, or near-duplicate above the configured Sorensen-Dice
//! threshold), so what the model reads is always a prefix-faithful selection of what the files
//! actually say, in the first file's voice. A fenced code block is one block — dedup never
//! splits code on the blank lines inside it.

/// One file's content, deduplicated against everything kept before it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileOut {
    pub name: String,
    pub kept: Vec<String>,
    /// How many of this file's blocks were dropped as duplicates of earlier content.
    pub dropped: usize,
}

/// Split markdown-ish text into blocks: paragraphs separated by blank lines, with a fenced code
/// block (``` … ```) kept whole however many blank lines it contains inside.
pub fn blocks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            cur.push(line);
            continue;
        }
        if line.trim().is_empty() && !in_fence {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
            continue;
        }
        cur.push(line);
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
}

/// The comparison key: lowercased, markdown list/heading lead-in stripped per line, whitespace
/// collapsed — so `- Ask when uncertain.` and `ask   when uncertain` count as the same statement.
pub fn normalize(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    for line in block.lines() {
        let line = line.trim_start();
        let line = line
            .trim_start_matches(['#', '-', '*', '>'])
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
            .trim();
        for word in line.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&word.to_lowercase());
        }
    }
    out
}

/// Whether `candidate` restates `kept`, at `threshold`: equality always; Sorensen-Dice on the
/// normalized text when the threshold admits near-duplicates (`threshold == 1.0` is exact-only).
fn restates(kept: &str, candidate: &str, threshold: f64) -> bool {
    if kept == candidate {
        return true;
    }
    threshold < 1.0 && strsim::sorensen_dice(kept, candidate) >= threshold
}

/// Dedup `files` (name, content) in order. Empty blocks never count as duplicates of each other;
/// an empty file yields an entry with nothing kept and nothing dropped.
pub fn dedup(files: &[(String, String)], threshold: f64) -> Vec<FileOut> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (name, content) in files {
        let mut kept = Vec::new();
        let mut dropped = 0usize;
        for block in blocks(content) {
            let key = normalize(&block);
            if key.is_empty() {
                continue;
            }
            if seen.iter().any(|k| restates(k, &key, threshold)) {
                dropped += 1;
                continue;
            }
            seen.push(key);
            kept.push(block);
        }
        out.push(FileOut {
            name: name.clone(),
            kept,
            dropped,
        });
    }
    out
}

/// Render the section body: each file under its own `## name` header, duplicates noted in one
/// line rather than repeated. A file whose every block was already stated collapses to the note;
/// a file that contributed nothing at all (empty) is omitted.
pub fn render_body(files: &[FileOut]) -> String {
    let mut out = String::new();
    for f in files {
        if f.kept.is_empty() && f.dropped == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("## {}\n", f.name));
        if f.kept.is_empty() {
            out.push_str("(everything in this file is already stated above)");
            continue;
        }
        out.push_str(&f.kept.join("\n\n"));
        if f.dropped > 0 {
            out.push_str(&format!(
                "\n\n({} block(s) omitted: already stated above)",
                f.dropped
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn a_fenced_code_block_is_one_block_whatever_blank_lines_it_holds() {
        let text = "para one\n\n```rust\nfn a() {}\n\nfn b() {}\n```\n\npara two";
        let b = blocks(text);
        assert_eq!(b.len(), 3, "{b:?}");
        assert!(b[1].contains("fn a() {}\n\nfn b() {}"), "{:?}", b[1]);
    }

    #[test]
    fn an_identical_second_file_collapses_to_the_note() {
        let content = "Be brief.\n\nAsk when uncertain.";
        let out = dedup(
            &files(&[("AGENTS.md", content), ("CLAUDE.md", content)]),
            1.0,
        );
        assert_eq!(out[0].kept.len(), 2);
        assert_eq!(out[1].kept.len(), 0);
        assert_eq!(out[1].dropped, 2);
        let body = render_body(&out);
        assert!(body.contains("## AGENTS.md\nBe brief."), "{body}");
        assert!(
            body.contains("## CLAUDE.md\n(everything in this file is already stated above)"),
            "{body}"
        );
    }

    #[test]
    fn a_restated_paragraph_is_dropped_and_a_new_one_is_kept() {
        let a = "- Be brief.\n\nPrefer ASI patterns.";
        let b = "Be   brief.\n\nNever push without asking.";
        let out = dedup(&files(&[("AGENTS.md", a), ("CLAUDE.md", b)]), 1.0);
        assert_eq!(out[1].kept, vec!["Never push without asking.".to_string()]);
        assert_eq!(out[1].dropped, 1, "the bullet restated `Be brief.`");
    }

    #[test]
    fn a_near_duplicate_is_dropped_at_the_threshold_and_kept_exact_only() {
        let a = "Always run the linter before committing your changes.";
        let b = "Always run the linter before you commit changes.";
        let near = dedup(&files(&[("A.md", a), ("B.md", b)]), 0.8);
        assert_eq!(near[1].dropped, 1, "a reworded copy is a duplicate at 0.8");
        let exact = dedup(&files(&[("A.md", a), ("B.md", b)]), 1.0);
        assert_eq!(exact[1].dropped, 0, "1.0 means exact-only");
    }

    #[test]
    fn dedup_also_holds_within_one_file() {
        let a = "Be brief.\n\nBe brief.";
        let out = dedup(&files(&[("A.md", a)]), 1.0);
        assert_eq!(out[0].kept.len(), 1);
        assert_eq!(out[0].dropped, 1);
    }

    #[test]
    fn an_empty_file_is_omitted_from_the_body() {
        let out = dedup(&files(&[("A.md", "hello"), ("B.md", "  \n\n")]), 1.0);
        let body = render_body(&out);
        assert!(!body.contains("## B.md"), "{body}");
    }
}
