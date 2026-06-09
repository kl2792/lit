/// BibTeX sanitization (ADR 001 change 1).
///
/// `bibtex::upsert_to_file` and `bibtex::append_to_file` run every entry
/// through `sanitize_bibtex` before writing, so `lit add`, `lit misc`, and
/// the global `-b/--bib` append path are covered by construction.
///
/// Transformations, per field value:
/// - HTML entities (named and numeric) decoded, then LaTeX-escaped:
///   `&` -> `\&`, `<`/`>` -> `\textless{}`/`\textgreater{}`.
/// - Unicode en/em dashes -> `--`/`---`.
/// - `month` field normalized to bare three-letter macros (`month = jun`);
///   unmappable values pass through unchanged with a warning.
///
/// Exemptions: `url` and `doi` fields are untouched; `\url{...}` spans inside
/// other fields are untouched. The pass is idempotent: its outputs contain
/// none of its inputs' trigger patterns.

use crate::bibtex::{parse_bib_file, BibEntry};

/// The twelve bare BibTeX month macros.
const MONTH_MACROS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Result of sanitizing a BibTeX string.
pub struct SanitizeOutcome {
    /// Sanitized BibTeX text (canonical serialization).
    pub text: String,
    /// Human-readable warnings (e.g. unmappable month values).
    pub warnings: Vec<String>,
    /// Whether the output differs from the (trimmed) input.
    pub changed: bool,
}

/// Returns true if `value` is a bare three-letter month macro.
pub fn is_month_macro(value: &str) -> bool {
    MONTH_MACROS.contains(&value)
}

/// Decode common named HTML entities to their plain-text equivalents.
///
/// Relocated from `api/openalex.rs` (ADR 001): shared by the OpenAlex title
/// cleanup and the BibTeX sanitize pass.
pub fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
}

/// Sanitize a BibTeX string (one or more entries).
///
/// Parses the input, transforms field values, and re-serializes in canonical
/// form. If the input contains no parseable entries it is returned unchanged.
pub fn sanitize_bibtex(input: &str) -> SanitizeOutcome {
    let entries = parse_bib_file(input);
    if entries.is_empty() {
        return SanitizeOutcome {
            text: input.to_string(),
            warnings: Vec::new(),
            changed: false,
        };
    }

    let mut warnings = Vec::new();
    let sanitized: Vec<String> = entries
        .into_iter()
        .map(|mut entry| {
            sanitize_entry_fields(&mut entry, &mut warnings);
            serialize_entry(&entry)
        })
        .collect();

    let text = sanitized.join("\n\n");
    let changed = text != input.trim();
    SanitizeOutcome { text, warnings, changed }
}

/// Transform all field values of an entry in place.
fn sanitize_entry_fields(entry: &mut BibEntry, warnings: &mut Vec<String>) {
    let key = entry.key.clone();
    for (name, value) in entry.fields.iter_mut() {
        if name == "url" || name == "doi" {
            continue;
        }
        if name == "month" {
            match map_month(value) {
                Some(macro_name) => *value = macro_name.to_string(),
                None => warnings.push(format!(
                    "{}: unmappable month value '{}' left unchanged",
                    key, value
                )),
            }
            continue;
        }
        *value = transform_value(value);
    }
}

/// Serialize an entry like `BibEntry`'s `Display`, but emit recognized month
/// macros bare (`month = jun`, no braces).
fn serialize_entry(entry: &BibEntry) -> String {
    let mut out = format!("@{}{{{},\n", entry.entry_type, entry.key);
    for (i, (name, value)) in entry.fields.iter().enumerate() {
        let comma = if i < entry.fields.len() - 1 { "," } else { "" };
        if name == "month" && is_month_macro(value) {
            out.push_str(&format!("  {} = {}{}\n", name, value, comma));
        } else {
            out.push_str(&format!("  {} = {{{}}}{}\n", name, value, comma));
        }
    }
    out.push('}');
    out
}

/// Map a month value to its bare macro, if recognizable.
///
/// Covers full names, three-letter macros, and common abbreviations
/// (`Sept`/`Sep.` -> `sep`), case-insensitively and ignoring a trailing dot.
fn map_month(value: &str) -> Option<&'static str> {
    let v = value.trim().trim_end_matches('.').to_lowercase();
    match v.as_str() {
        "january" | "jan" => Some("jan"),
        "february" | "feb" => Some("feb"),
        "march" | "mar" => Some("mar"),
        "april" | "apr" => Some("apr"),
        "may" => Some("may"),
        "june" | "jun" => Some("jun"),
        "july" | "jul" => Some("jul"),
        "august" | "aug" => Some("aug"),
        "september" | "sept" | "sep" => Some("sep"),
        "october" | "oct" => Some("oct"),
        "november" | "nov" => Some("nov"),
        "december" | "dec" => Some("dec"),
        _ => None,
    }
}

/// Sanitize a single field value (entities, dashes, escaping); used by the
/// collision guard to compare titles on equal footing when the existing
/// entry predates the sanitize pass.
pub(crate) fn sanitize_field_value(value: &str) -> String {
    transform_value(value)
}

/// Transform a field value, leaving `\url{...}` spans verbatim.
fn transform_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(idx) = rest.find("\\url{") {
        out.push_str(&transform_segment(&rest[..idx]));
        let bytes = rest.as_bytes();
        let mut pos = idx + 5; // past "\url{"
        let mut depth = 1;
        while pos < bytes.len() && depth > 0 {
            match bytes[pos] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            pos += 1;
        }
        out.push_str(&rest[idx..pos]);
        rest = &rest[pos..];
    }
    out.push_str(&transform_segment(rest));
    out
}

/// Decode entities, normalize dashes, then LaTeX-escape.
fn transform_segment(s: &str) -> String {
    let decoded = decode_numeric_entities(&decode_html_entities(s));
    latex_escape(&normalize_dashes(&decoded))
}

/// Decode `&#NNN;` and `&#xHH;` numeric entities.
fn decode_numeric_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("&#") {
        let after = &rest[start + 2..];
        let (hex, digits_start) = if after.starts_with('x') || after.starts_with('X') {
            (true, 1)
        } else {
            (false, 0)
        };
        let decoded = after[digits_start..].find(';').and_then(|semi| {
            let digits = &after[digits_start..digits_start + semi];
            if digits.is_empty() {
                return None;
            }
            let code = if hex {
                u32::from_str_radix(digits, 16).ok()?
            } else {
                digits.parse::<u32>().ok()?
            };
            char::from_u32(code).map(|c| (c, digits_start + semi + 1))
        });
        match decoded {
            Some((c, consumed)) => {
                out.push_str(&rest[..start]);
                out.push(c);
                rest = &after[consumed..];
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Normalize Unicode en/em dashes to LaTeX `--`/`---`.
fn normalize_dashes(s: &str) -> String {
    s.replace('\u{2014}', "---").replace('\u{2013}', "--")
}

/// Escape `&` (unless already escaped), `<`, and `>` for LaTeX.
fn latex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_backslash = false;
    for c in s.chars() {
        match c {
            '&' if !prev_backslash => out.push_str("\\&"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            _ => out.push(c),
        }
        prev_backslash = c == '\\';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("A &amp; B"), "A & B");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("it&#39;s"), "it's");
    }

    #[test]
    fn test_decode_numeric_entities() {
        assert_eq!(decode_numeric_entities("A &#38; B"), "A & B");
        assert_eq!(decode_numeric_entities("A &#x26; B"), "A & B");
        assert_eq!(decode_numeric_entities("no entity &# here"), "no entity &# here");
    }

    #[test]
    fn test_map_month() {
        assert_eq!(map_month("June"), Some("jun"));
        assert_eq!(map_month("Sept"), Some("sep"));
        assert_eq!(map_month("Sep."), Some("sep"));
        assert_eq!(map_month("jun"), Some("jun"));
        assert_eq!(map_month("June 2020"), None);
    }

    #[test]
    fn test_latex_escape_skips_escaped() {
        assert_eq!(latex_escape(r"A \& B & C"), r"A \& B \& C");
    }

    #[test]
    fn test_unparseable_input_passes_through() {
        let out = sanitize_bibtex("not bibtex at all");
        assert_eq!(out.text, "not bibtex at all");
        assert!(!out.changed);
    }
}
