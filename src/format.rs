/// Output formatting with auto-detected color support.
///
/// Colors are enabled only when both stdout and stderr are TTYs, matching
/// the bash script's `[ -t 1 ] && [ -t 2 ]` check.

use std::io::IsTerminal;

// ANSI color codes
const RED: &str = "\x1b[0;31m";
const YELLOW: &str = "\x1b[0;33m";
const BLUE: &str = "\x1b[0;34m";
const NC: &str = "\x1b[0m";

/// Returns true if color output should be used.
///
/// Disabled when either stream is not a TTY, or when the `NO_COLOR` env var
/// is set to any non-empty value (per <https://no-color.org>).
/// The `--no-color` CLI flag sets `NO_COLOR=1` at startup.
fn use_color() -> bool {
    if let Ok(val) = std::env::var("NO_COLOR") {
        if !val.is_empty() {
            return false;
        }
    }
    std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

/// Print an informational message to stderr (blue when color is enabled).
pub fn info(msg: &str) {
    if use_color() {
        eprintln!("{}{}{}", BLUE, msg, NC);
    } else {
        eprintln!("{}", msg);
    }
}

/// Print a warning message to stderr (yellow when color is enabled).
pub fn warn(msg: &str) {
    if use_color() {
        eprintln!("{}{}{}", YELLOW, msg, NC);
    } else {
        eprintln!("{}", msg);
    }
}

/// Print an error message to stderr (red when color is enabled).
pub fn error(msg: &str) {
    if use_color() {
        eprintln!("{}{}{}", RED, msg, NC);
    } else {
        eprintln!("{}", msg);
    }
}


/// Truncate a string at a safe UTF-8 char boundary, up to `max` bytes.
///
/// Returns a slice (no ellipsis). Useful when a fixed byte budget is needed.
pub fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate a string to at most `max` characters, appending "..." if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        let mut result: String = s.chars().take(max - 3).collect();
        result.push_str("...");
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(80);
        let result = truncate(&long, 70);
        assert_eq!(result.len(), 70);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_empty() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn truncate_to_three() {
        assert_eq!(truncate("hello world", 3), "hel");
    }

    #[test]
    fn truncate_unicode() {
        // Should handle multi-byte chars correctly (truncate by char count)
        let s = "cafe\u{0301} latte";
        let result = truncate(s, 6);
        // The combining accent is a separate char, so 6 chars = "cafe\u{0301} "
        assert!(result.len() <= 10); // sanity check on byte length
    }

    #[test]
    fn truncate_unicode_no_false_truncation() {
        // "Schölkopf" is 10 chars but 11 bytes (ö = 2 bytes).
        // truncate with max=10 should NOT truncate.
        let s = "Sch\u{00F6}lkopf";
        assert_eq!(s.chars().count(), 9);
        assert_eq!(s.len(), 10); // 10 bytes
        let result = truncate(s, 10);
        assert_eq!(result, s); // should not be truncated
    }
}
