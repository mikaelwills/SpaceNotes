/// Sanitizes a file path to be URI-safe by replacing problematic Unicode characters
/// with their ASCII equivalents or safe alternatives.
///
/// This prevents URI encoding/decoding issues in clients (especially Flutter)
/// when paths contain characters like smart quotes, ellipsis, etc.
///
/// # Examples
/// ```
/// let path = "folder/Note with … ellipsis.md";
/// let sanitized = sanitize_path(path);
/// assert_eq!(sanitized, "folder/Note with ... ellipsis.md");
/// ```
pub fn sanitize_path(path: &str) -> String {
    path
        // Replace ellipsis with three dots
        .replace('\u{2026}', "...")
        // Replace smart quotes with regular quotes
        .replace('\u{201C}', "\"")  // Left double quote
        .replace('\u{201D}', "\"")  // Right double quote
        .replace('\u{2018}', "'")   // Left single quote
        .replace('\u{2019}', "'")   // Right single quote
        // Replace em dash and en dash with regular dash
        .replace('\u{2014}', "-")   // Em dash
        .replace('\u{2013}', "-")   // En dash
        .chars()
        .map(|c| {
            // Keep ASCII alphanumeric, forward slash (for paths), spaces, and safe punctuation
            if c.is_ascii_alphanumeric() || "/. -_,()[]\"'".contains(c) {
                c
            } else {
                // Replace any other character with underscore
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_ellipsis() {
        let input = "Sing the chorus low and ethereal over the intro before the current….md";
        let expected = "Sing the chorus low and ethereal over the intro before the current....md";
        assert_eq!(sanitize_path(input), expected);
    }

    #[test]
    fn test_sanitize_smart_quotes() {
        let input = "folder/\u{201C}Smart\u{201D} quotes \u{2018}here\u{2019}.md";
        let expected = "folder/\"Smart\" quotes 'here'.md";
        assert_eq!(sanitize_path(input), expected);
    }

    #[test]
    fn test_sanitize_dashes() {
        let input = "Note with\u{2014}em dash and\u{2013}en dash.md";
        let expected = "Note with-em dash and-en dash.md";
        assert_eq!(sanitize_path(input), expected);
    }

    #[test]
    fn test_preserve_path_separators() {
        let input = "Development/Projects/My \u{201C}Project\u{201D}.md";
        let expected = "Development/Projects/My \"Project\".md";
        assert_eq!(sanitize_path(input), expected);
    }

    #[test]
    fn test_replace_unknown_unicode() {
        let input = "Note with emoji 🎵 and symbols ©.md";
        let expected = "Note with emoji _ and symbols _.md";
        assert_eq!(sanitize_path(input), expected);
    }

    #[test]
    fn test_clean_path_unchanged() {
        let input = "Development/Clean-File_Name.md";
        assert_eq!(sanitize_path(input), input);
    }
}
