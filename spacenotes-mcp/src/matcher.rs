use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Exact,
    TrailingWhitespace,
    Indentation,
    Block,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Exact => "exact",
            Tier::TrailingWhitespace => "trailing-whitespace-relaxed",
            Tier::Indentation => "indentation-relaxed",
            Tier::Block => "block-relaxed",
        }
    }

    // Tier 3 folds Unicode and collapses whitespace runs, so it can match text the caller did
    // not mean. Gated behind uniqueness or an explicit opt-in.
    pub fn needs_opt_in(self) -> bool {
        matches!(self, Tier::Block)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub first_line: usize,
    pub last_line: usize,
}

#[derive(Debug, Clone)]
pub struct Matches {
    pub tier: Tier,
    pub matches: Vec<Match>,
}

struct Line {
    start: usize,
    end: usize,
    text: String,
}

fn lines_with_offsets(content: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            let mut end = idx;
            if end > start && content.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(Line {
                start,
                end,
                text: content[start..end].to_string(),
            });
            start = idx + 1;
        }
    }
    if start <= content.len() {
        let rest = &content[start..];
        if !rest.is_empty() || content.is_empty() {
            lines.push(Line {
                start,
                end: content.len(),
                text: rest.to_string(),
            });
        }
    }
    lines
}

fn fold_unicode(s: &str) -> Cow<'_, str> {
    if s.chars().any(|c| matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')) {
        Cow::Owned(
            s.chars()
                .map(|c| match c {
                    '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
                    other => other,
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(s)
    }
}

fn collapse_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

fn compare_key(line: &str, tier: Tier) -> String {
    match tier {
        Tier::Exact => line.to_string(),
        Tier::TrailingWhitespace => line.trim_end().to_string(),
        Tier::Indentation => line.trim().to_string(),
        Tier::Block => collapse_runs(fold_unicode(line).trim()),
    }
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

pub fn find(content: &str, needle: &str) -> Option<Matches> {
    if needle.is_empty() {
        return None;
    }

    if let Some(found) = find_exact(content, needle) {
        return Some(Matches {
            tier: Tier::Exact,
            matches: found,
        });
    }

    for tier in [Tier::TrailingWhitespace, Tier::Indentation, Tier::Block] {
        let found = find_by_lines(content, needle, tier);
        if !found.is_empty() {
            return Some(Matches { tier, matches: found });
        }
    }
    None
}

fn line_of(content: &str, byte: usize) -> usize {
    content[..byte].matches('\n').count() + 1
}

fn find_exact(content: &str, needle: &str) -> Option<Vec<Match>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = content[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        out.push(Match {
            start,
            end,
            first_line: line_of(content, start),
            last_line: line_of(content, end.saturating_sub(1)),
        });
        from = end.max(start + 1);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn find_by_lines(content: &str, needle: &str, tier: Tier) -> Vec<Match> {
    let haystack = lines_with_offsets(content);
    let needle_lines: Vec<&str> = needle.lines().collect();
    if needle_lines.is_empty() || haystack.is_empty() {
        return Vec::new();
    }

    let keys: Vec<String> = haystack.iter().map(|l| compare_key(&l.text, tier)).collect();
    let want: Vec<String> = needle_lines.iter().map(|l| compare_key(l, tier)).collect();

    // Tier 3 treats any run of blank lines as one separator, so a needle with one blank line
    // matches a note with three.
    let equal = |a: &str, b: &str| {
        if tier == Tier::Block && is_blank(a) && is_blank(b) {
            true
        } else {
            a == b
        }
    };

    let mut out = Vec::new();
    if want.len() > keys.len() {
        return out;
    }
    for offset in 0..=(keys.len() - want.len()) {
        if want
            .iter()
            .enumerate()
            .all(|(i, w)| equal(&keys[offset + i], w))
        {
            let first = &haystack[offset];
            let last = &haystack[offset + want.len() - 1];
            out.push(Match {
                start: first.start,
                end: last.end,
                first_line: offset + 1,
                last_line: offset + want.len(),
            });
        }
    }
    out
}

fn dominant_terminator(content: &str) -> &'static str {
    let crlf = content.matches("\r\n").count();
    let lf = content.matches('\n').count() - crlf;
    if crlf > lf {
        "\r\n"
    } else {
        "\n"
    }
}

fn indent_of(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    &line[..end]
}

// Reconciles the replacement with the matched region's real indentation and line endings, so a
// tier-2 match into indented text keeps that indentation.
fn reconcile(content: &str, m: &Match, needle: &str, replacement: &str, tier: Tier) -> String {
    let terminator = dominant_terminator(content);
    let normalized: Vec<&str> = replacement.lines().collect();

    let body = if matches!(tier, Tier::Indentation | Tier::Block) {
        let actual_indent = indent_of(&content[m.start..m.end].lines().next().unwrap_or(""));
        let needle_indent = indent_of(needle.lines().next().unwrap_or(""));
        normalized
            .iter()
            .map(|line| {
                let stripped = line.strip_prefix(needle_indent).unwrap_or(line);
                format!("{}{}", actual_indent, stripped)
            })
            .collect::<Vec<_>>()
            .join(terminator)
    } else {
        normalized.join(terminator)
    };

    if replacement.ends_with('\n') && !body.is_empty() {
        format!("{}{}", body, terminator)
    } else {
        body
    }
}

pub fn apply(
    content: &str,
    needle: &str,
    replacement: &str,
    found: &Matches,
    targets: &[usize],
) -> String {
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for &idx in targets {
        let m = &found.matches[idx];
        out.push_str(&content[cursor..m.start]);
        out.push_str(&reconcile(content, m, needle, replacement, found.tier));
        cursor = m.end;

        // A whole-line match deleted to nothing would otherwise leave a blank line behind.
        if replacement.is_empty() && content[m.start..].starts_with(&content[m.start..m.end]) {
            let after = &content[m.end..];
            if let Some(rest) = after.strip_prefix("\r\n") {
                if is_whole_lines(content, m) {
                    cursor = m.end + (after.len() - rest.len());
                }
            } else if let Some(rest) = after.strip_prefix('\n') {
                if is_whole_lines(content, m) {
                    cursor = m.end + (after.len() - rest.len());
                }
            }
        }
    }
    out.push_str(&content[cursor..]);
    out
}

fn is_whole_lines(content: &str, m: &Match) -> bool {
    let starts_line = m.start == 0 || content[..m.start].ends_with('\n');
    let ends_line = m.end == content.len()
        || content[m.end..].starts_with('\n')
        || content[m.end..].starts_with("\r\n");
    starts_line && ends_line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(content: &str, needle: &str) -> Matches {
        find(content, needle).expect("expected a match")
    }

    #[test]
    fn tier0_matches_exact_bytes() {
        let found = only("alpha\nbeta\n", "beta");
        assert_eq!(found.tier, Tier::Exact);
        assert_eq!(found.matches.len(), 1);
    }

    #[test]
    fn tier0_counts_every_occurrence() {
        let found = only("a\nb\na\n", "a");
        assert_eq!(found.matches.len(), 2);
    }

    #[test]
    fn tier1_ignores_trailing_whitespace() {
        let found = only("alpha  \nbeta\n", "alpha\nbeta");
        assert_eq!(found.tier, Tier::TrailingWhitespace);
    }

    #[test]
    fn tier1_ignores_crlf() {
        let found = only("alpha\r\nbeta\r\n", "alpha\nbeta");
        assert_eq!(found.tier, Tier::TrailingWhitespace);
    }

    #[test]
    fn tier2_ignores_indentation() {
        let found = only("    alpha\n    beta\n", "alpha\nbeta");
        assert_eq!(found.tier, Tier::Indentation);
    }

    #[test]
    fn tier2_ignores_tabs_vs_spaces_in_indent() {
        let found = only("\talpha\n", "    alpha");
        assert_eq!(found.tier, Tier::Indentation);
    }

    #[test]
    fn tier3_folds_nbsp() {
        let found = only("alpha\u{00A0}beta\n", "alpha beta");
        assert_eq!(found.tier, Tier::Block);
    }

    #[test]
    fn tier3_treats_a_whitespace_only_line_as_blank() {
        let found = only("a\n   \nb\n", "a\n\nb");
        assert_eq!(found.tier, Tier::TrailingWhitespace);
    }

    #[test]
    fn empty_needle_never_matches() {
        assert!(find("abc", "").is_none());
    }

    #[test]
    fn absent_needle_returns_none() {
        assert!(find("alpha\n", "zebra").is_none());
    }

    #[test]
    fn apply_preserves_untouched_bytes_and_final_newline() {
        let content = "keep  \nalpha\nkeep2  \n";
        let found = only(content, "alpha");
        let out = apply(content, "alpha", "OMEGA", &found, &[0]);
        assert_eq!(out, "keep  \nOMEGA\nkeep2  \n");
    }

    #[test]
    fn apply_does_not_normalize_the_rest_of_the_note() {
        // The old fallback trimmed every line; splicing must not.
        let content = "hard break  \r\ntarget\r\nmore  \r\n";
        let found = only(content, "target");
        let out = apply(content, "target", "X", &found, &[0]);
        assert!(out.contains("hard break  \r\n"));
        assert!(out.contains("more  \r\n"));
    }

    #[test]
    fn apply_converts_replacement_to_the_notes_terminator() {
        let content = "a\r\nb\r\n";
        let found = only(content, "b");
        let out = apply(content, "b", "x\ny", &found, &[0]);
        assert!(out.contains("x\r\ny"), "got {:?}", out);
    }

    #[test]
    fn apply_reindents_a_tier2_match() {
        let content = "    alpha\n    beta\n";
        let found = only(content, "alpha\nbeta");
        assert_eq!(found.tier, Tier::Indentation);
        let out = apply(content, "alpha\nbeta", "gamma\ndelta", &found, &[0]);
        assert_eq!(out, "    gamma\n    delta\n");
    }

    #[test]
    fn apply_replaces_all_targets_left_to_right() {
        let content = "a\nb\na\n";
        let found = only(content, "a");
        let out = apply(content, "a", "Z", &found, &[0, 1]);
        assert_eq!(out, "Z\nb\nZ\n");
    }

    #[test]
    fn apply_then_revert_is_byte_identical() {
        let content = "alpha  \r\nbeta\r\n\tgamma\r\n";
        let found = only(content, "beta");
        let edited = apply(content, "beta", "BETA", &found, &[0]);
        let back = find(&edited, "BETA").expect("match");
        let reverted = apply(&edited, "BETA", "beta", &back, &[0]);
        assert_eq!(reverted, content);
    }

    #[test]
    fn deleting_whole_lines_leaves_no_blank_residue() {
        let content = "a\ndrop\nb\n";
        let found = only(content, "drop");
        let out = apply(content, "drop", "", &found, &[0]);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn deleting_inside_a_line_keeps_the_line() {
        let content = "keep-drop-keep\n";
        let found = only(content, "drop");
        let out = apply(content, "drop", "", &found, &[0]);
        assert_eq!(out, "keep--keep\n");
    }

    #[test]
    fn tier3_is_flagged_as_needing_opt_in() {
        assert!(Tier::Block.needs_opt_in());
        assert!(!Tier::Exact.needs_opt_in());
        assert!(!Tier::Indentation.needs_opt_in());
    }

    #[test]
    fn match_reports_line_numbers() {
        let found = only("one\ntwo\nthree\n", "two");
        assert_eq!(found.matches[0].first_line, 2);
        assert_eq!(found.matches[0].last_line, 2);
    }

    #[test]
    fn multiline_match_reports_a_line_range() {
        let found = only("one\ntwo\nthree\n", "two\nthree");
        assert_eq!(found.matches[0].first_line, 2);
        assert_eq!(found.matches[0].last_line, 3);
    }

    #[test]
    fn lines_with_offsets_handles_no_final_newline() {
        let lines = lines_with_offsets("a\nb");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "b");
    }

    #[test]
    fn dominant_terminator_prefers_the_majority() {
        assert_eq!(dominant_terminator("a\r\nb\r\nc\n"), "\r\n");
        assert_eq!(dominant_terminator("a\nb\n"), "\n");
        assert_eq!(dominant_terminator("no newlines"), "\n");
    }
}

#[cfg(test)]
mod adversarial {
    use super::*;

    #[test]
    fn replace_all_with_different_length_replacement_stays_correct() {
        // Offsets come from the ORIGINAL content; a longer replacement must not corrupt later ones.
        let content = "x\nx\nx\n";
        let found = find(content, "x").unwrap();
        let out = apply(content, "x", "LONGER", &found, &[0, 1, 2]);
        assert_eq!(out, "LONGER\nLONGER\nLONGER\n");
    }

    #[test]
    fn replace_all_with_shorter_replacement_stays_correct() {
        let content = "aaa\nbbb\naaa\n";
        let found = find(content, "aaa").unwrap();
        let out = apply(content, "aaa", "z", &found, &[0, 1]);
        assert_eq!(out, "z\nbbb\nz\n");
    }

    #[test]
    fn match_at_byte_zero() {
        let content = "start\nrest\n";
        let found = find(content, "start").unwrap();
        assert_eq!(found.matches[0].start, 0);
        assert_eq!(apply(content, "start", "S", &found, &[0]), "S\nrest\n");
    }

    #[test]
    fn match_at_eof_without_trailing_newline() {
        let content = "a\nlast";
        let found = find(content, "last").unwrap();
        assert_eq!(apply(content, "last", "END", &found, &[0]), "a\nEND");
    }

    #[test]
    fn multiline_replacement_into_single_line_match() {
        let content = "a\ntarget\nb\n";
        let found = find(content, "target").unwrap();
        assert_eq!(apply(content, "target", "1\n2", &found, &[0]), "a\n1\n2\nb\n");
    }

    #[test]
    fn overlapping_needle_does_not_double_count_bytes() {
        let content = "aaaa\n";
        let found = find(content, "aa").unwrap();
        let out = apply(content, "aa", "b", &found, &[0, 1]);
        assert_eq!(out, "bb\n", "got {:?} from {} matches", out, found.matches.len());
    }

    #[test]
    fn deleting_the_only_line_empties_the_note() {
        let content = "solo\n";
        let found = find(content, "solo").unwrap();
        assert_eq!(apply(content, "solo", "", &found, &[0]), "");
    }
}
