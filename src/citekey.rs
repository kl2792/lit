//! Citation key generation.
//!
//! Generates BibTeX citation keys in the format `lastname2017firstword`,
//! matching Google Scholar style.

use std::sync::LazyLock;

use regex::Regex;

use crate::api::extract_last_name;

static ALPHA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z]").unwrap());
static STRIP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z\s]").unwrap());

/// Words to skip when selecting the first significant title word.
pub const SKIP_WORDS: &[&str] = &[
    "the", "a", "an", "of", "and", "in", "on", "for", "to", "how", "what", "why", "when",
    "with", "from", "by", "is", "are", "at", "its",
];

/// Generate a citation key from author list, year, and title.
///
/// Format: `{last_name_of_first_author}{year}{first_significant_title_word}`
///
/// - Last name: last whitespace-delimited token of first author, lowercased,
///   non-alpha characters stripped.
/// - Year: used as-is (should be 4 digits).
/// - Title word: first word not in SKIP_WORDS and longer than 2 characters,
///   after stripping non-alphanumeric, lowercased. If no qualifying word,
///   empty string.
pub fn generate(authors: &[String], year: &str, title: &str) -> String {
    // Extract last name of first author
    let last_name = if let Some(first_author) = authors.first() {
        let last = extract_last_name(first_author.trim());
        // Strip non-alpha characters and lowercase
        ALPHA_RE.replace_all(last, "").to_lowercase()
    } else {
        "unknown".to_string()
    };

    // Find first significant title word (must be >2 chars and not a skip word)
    let cleaned_title = STRIP_RE.replace_all(title, "");
    let title_word = cleaned_title
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .find(|w| !SKIP_WORDS.contains(&w.as_str()) && w.len() > 2)
        .unwrap_or_default();

    format!("{}{}{}", last_name, year, title_word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_citekey() {
        let authors = vec!["Jonathan Ho".to_string()];
        let key = generate(&authors, "2020", "Denoising Diffusion Probabilistic Models");
        assert_eq!(key, "ho2020denoising");
    }

    #[test]
    fn skip_common_words() {
        let authors = vec!["Ashish Vaswani".to_string()];
        let key = generate(&authors, "2017", "Attention Is All You Need");
        assert_eq!(key, "vaswani2017attention");
    }

    #[test]
    fn skip_leading_articles() {
        let authors = vec!["John Smith".to_string()];
        let key = generate(&authors, "2021", "The Art of Reasoning");
        assert_eq!(key, "smith2021art");
    }

    #[test]
    fn multiple_authors_uses_first() {
        let authors = vec![
            "Amir-Hossein Karimi".to_string(),
            "Bernhard Scholkopf".to_string(),
        ];
        let key = generate(&authors, "2021", "Algorithmic Recourse Under Imperfect Information");
        assert_eq!(key, "karimi2021algorithmic");
    }

    #[test]
    fn empty_authors() {
        let authors: Vec<String> = vec![];
        let key = generate(&authors, "2021", "Some Paper Title");
        assert_eq!(key, "unknown2021some");
    }

    #[test]
    fn title_with_special_chars() {
        let authors = vec!["Scott Lundberg".to_string()];
        let key = generate(
            &authors,
            "2017",
            "A Unified Approach to Interpreting Model Predictions",
        );
        assert_eq!(key, "lundberg2017unified");
    }

    #[test]
    fn all_skip_words_title() {
        let authors = vec!["Jane Doe".to_string()];
        let key = generate(&authors, "2020", "of the and in on");
        assert_eq!(key, "doe2020");
    }

    #[test]
    fn author_with_hyphen() {
        let authors = vec!["Amir-Hossein Karimi".to_string()];
        let key = generate(&authors, "2021", "Algorithmic Recourse");
        assert_eq!(key, "karimi2021algorithmic");
    }

    #[test]
    fn title_with_numbers() {
        let authors = vec!["Ilya Sutskever".to_string()];
        let key = generate(&authors, "2014", "Sequence to Sequence Learning with Neural Networks");
        assert_eq!(key, "sutskever2014sequence");
    }
}
