/// Normalizes a vault-relative path for storage.
///
/// The vault is ground truth: the stored path must match the bytes on disk, or the daemon
/// cannot read the file back. So this preserves the filename verbatim — including Unicode
/// (em dash, smart quotes, ellipsis, accents, emoji) — and strips only control characters,
/// which are never legal in a path.
///
/// URI safety is the client's job, not this function's: the Flutter client wraps every path
/// segment in `Uri.encodeComponent` when building a URL and decodes per-segment on the way
/// back, and notes are routed by id rather than path.
///
/// # Examples
/// ```
/// let path = "folder/File with … ellipsis.md";
/// assert_eq!(sanitize_path(path), path);
/// ```
pub fn sanitize_path(path: &str) -> String {
    path.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ellipsis_is_preserved() {
        let input = "Sing the chorus low and ethereal over the intro before the current\u{2026}.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_smart_quotes_are_preserved() {
        let input = "folder/\u{201C}Smart\u{201D} quotes \u{2018}here\u{2019}.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_dashes_are_preserved() {
        let input = "File with\u{2014}em dash and\u{2013}en dash.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_preserve_path_separators() {
        let input = "Development/Projects/My \u{201C}Project\u{201D}.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_non_ascii_is_preserved() {
        let input = "File with emoji \u{1F3B5} and symbols \u{00A9}.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_clean_path_unchanged() {
        let input = "Development/Clean-File_Name.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_ampersand_is_preserved() {
        let input = "Philosophy & Ideas/Satanism.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_common_punctuation_is_preserved() {
        let input = "Food/M&S vs Sainsburys - 100% Beef (Tortellini) @ 5!.md";
        assert_eq!(sanitize_path(input), input);
    }

    #[test]
    fn test_control_characters_are_stripped() {
        let input = "folder/name\u{0000}with\u{001F}control\u{007F}chars.md";
        assert_eq!(sanitize_path(input), "folder/namewithcontrolchars.md");
    }

    #[test]
    fn test_newline_is_stripped() {
        assert_eq!(sanitize_path("folder/na\nme.md"), "folder/name.md");
    }
}
