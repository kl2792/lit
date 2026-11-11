//! Credential-shaped strings: the single pattern list behind capture-time
//! redaction (`cmd::supplements`) and the repository scan
//! (`tests/secret_scan.rs`).
//!
//! Matching is by shape only. A publisher's public browser key matches exactly
//! like a private credential, because a committed fixture cannot tell them apart.

use regex::bytes::Regex;
use std::ops::Range;
use std::sync::OnceLock;

/// Named credential patterns. Each name is upper case so that its redaction
/// placeholder (`REDACTED_<NAME>_XXX...`) matches no pattern.
pub const PATTERNS: &[(&str, &str)] = &[
    ("GOOGLE_API_KEY", r"AIza[0-9A-Za-z_\-]{35}"),
    ("AWS_ACCESS_KEY_ID", r"(?:AKIA|ASIA)[0-9A-Z]{16}"),
    ("GITHUB_TOKEN", r"gh[pousr]_[0-9A-Za-z]{36,}"),
    ("GITHUB_PAT", r"github_pat_[0-9A-Za-z_]{80,}"),
    ("SLACK_TOKEN", r"xox[abposr]-[0-9A-Za-z\-]{10,}"),
    // The word boundary keeps English slugs such as `risk-assessments-...` out.
    ("SECRET_KEY", r"\bsk-(?:ant-)?[0-9A-Za-z_\-]{32,}"),
    ("PRIVATE_KEY", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
];

fn compiled() -> &'static [(&'static str, Regex)] {
    static COMPILED: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| PATTERNS.iter().map(|(name, pattern)| (*name, Regex::new(pattern).expect("valid credential pattern"))).collect())
}

/// Every credential-shaped match in `bytes` as (pattern name, byte range),
/// ordered by start offset. Matches of different patterns may overlap.
pub fn find(bytes: &[u8]) -> Vec<(&'static str, Range<usize>)> {
    let mut found: Vec<_> = compiled().iter()
        .flat_map(|(name, regex)| regex.find_iter(bytes).map(move |m| (*name, m.range())))
        .collect();
    found.sort_by_key(|(_, range)| range.start);
    found
}

/// `REDACTED_<NAME>_` padded with `X` or cut to `len` bytes. Every pattern needs
/// at least 15 bytes and a prefix that never occurs in the placeholder.
fn placeholder(name: &str, len: usize) -> Vec<u8> {
    let mut bytes = format!("REDACTED_{name}_").into_bytes();
    bytes.resize(len, b'X');
    bytes
}

/// Overwrites each match in place with a same-length placeholder, so byte
/// lengths and offsets are unchanged. Returns the redacted bytes and the number
/// of matches replaced.
pub fn redact(bytes: &[u8]) -> (Vec<u8>, usize) {
    let found = find(bytes);
    let mut out = bytes.to_vec();
    for (name, range) in &found {
        out[range.clone()].copy_from_slice(&placeholder(name, range.len()));
    }
    (out, found.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sample per pattern, built at run time so this source file itself
    /// holds no credential-shaped string for the repository scan to flag.
    fn samples() -> Vec<(&'static str, String)> {
        vec![
            ("GOOGLE_API_KEY", format!("AIza{}", "a1_-".repeat(8) + "b2c")),
            ("AWS_ACCESS_KEY_ID", format!("AKIA{}", "ABCD2345EFGH6789")),
            ("AWS_ACCESS_KEY_ID", format!("ASIA{}", "Z".repeat(16))),
            ("GITHUB_TOKEN", format!("ghp_{}", "a".repeat(36))),
            ("GITHUB_TOKEN", format!("ghs_{}", "Z9".repeat(20))),
            ("GITHUB_PAT", format!("github_pat_{}", "a_1".repeat(27))),
            ("SLACK_TOKEN", format!("xoxb-{}", "123-abc-DEF")),
            ("SECRET_KEY", format!("sk-{}", "a".repeat(48))),
            ("SECRET_KEY", format!("sk-ant-{}", "api03_".repeat(8))),
            ("PRIVATE_KEY", format!("-----BEGIN {}PRIVATE KEY-----", "RSA ")),
            ("PRIVATE_KEY", format!("-----BEGIN {}PRIVATE KEY-----", "")),
        ]
    }

    #[test]
    fn every_pattern_finds_its_sample_inside_surrounding_text() {
        for (name, sample) in samples() {
            let text = format!("config={{key:\"{sample}\"}}\n");
            let found = find(text.as_bytes());
            assert_eq!(found.len(), 1, "{name}: {found:?}");
            assert_eq!(found[0].0, name);
            assert_eq!(&text.as_bytes()[found[0].1.clone()], sample.as_bytes());
        }
    }

    #[test]
    fn near_misses_do_not_match() {
        let near = [
            format!("AIza{}", "a".repeat(34)),
            format!("AKIA{}", "abcd".repeat(4)),
            format!("ghp_{}", "a".repeat(35)),
            format!("github_pat_{}", "a".repeat(79)),
            format!("xoxb-{}", "a".repeat(9)),
            format!("sk-{}", "a".repeat(31)),
            format!("machine-bias-ri{}{}", "sk-", "assessments-in-criminal-sentencing"),
            "-----BEGIN PUBLIC KEY-----".to_string(),
            "-----BEGIN CERTIFICATE-----".to_string(),
        ];
        for text in near {
            assert!(find(text.as_bytes()).is_empty(), "{text}");
        }
    }

    #[test]
    fn redact_replaces_every_match_with_a_same_length_placeholder_that_matches_nothing() {
        let text: String = samples().iter().map(|(_, s)| format!("<{s}>\n")).collect();
        let (out, count) = redact(text.as_bytes());
        assert_eq!(count, samples().len());
        assert_eq!(out.len(), text.len());
        assert!(find(&out).is_empty(), "{}", String::from_utf8_lossy(&out));
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        for ((name, sample), line) in samples().iter().zip(lines) {
            assert_eq!(line.len(), sample.len() + 2);
            let expected = format!("REDACTED_{name}_");
            let shown = &line[1..line.len() - 1];
            assert!(expected.starts_with(shown) || shown.starts_with(&expected), "{name}: {shown}");
        }
    }

    #[test]
    fn google_key_placeholder_is_the_documented_fixture_value() {
        let key = format!("AIza{}", "x".repeat(35));
        assert_eq!(redact(key.as_bytes()), (b"REDACTED_GOOGLE_API_KEY_XXXXXXXXXXXXXXX".to_vec(), 1));
    }

    #[test]
    fn redact_leaves_text_without_credentials_unchanged() {
        let text = b"<script>window.__NUXT__.config={public:{site:\"frontiers\"}}</script>";
        assert_eq!(redact(text), (text.to_vec(), 0));
    }
}
