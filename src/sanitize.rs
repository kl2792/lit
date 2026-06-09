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
/// other fields are untouched. The pass is idempotent: entity decoding strips
/// exactly one encoding level per pass, and an ampersand preceded by a
/// backslash (the LaTeX-escaped output of a previous pass) never starts an
/// entity, so sanitized text is a fixed point.

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
/// Single left-to-right scan: decoded output is never rescanned, so
/// double-encoded input loses exactly one encoding level per call
/// (`&amp;amp;` -> `&amp;`, never `&`) and replacements cannot create new
/// trigger patterns mid-pass. An `&` preceded by a backslash is left alone:
/// that is the LaTeX-escaped output of a previous sanitize pass, which keeps
/// `sanitize_bibtex` idempotent.
///
/// Relocated from `api/openalex.rs` (ADR 001): shared by the OpenAlex title
/// cleanup and the BibTeX sanitize pass.
pub fn decode_html_entities(s: &str) -> String {
    const ENTITIES: [(&str, &str); 7] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&#x27;", "'"),
        ("&apos;", "'"),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if rest.starts_with('&')
            && !out.ends_with('\\')
            && let Some((entity, plain)) = ENTITIES.iter().find(|(e, _)| rest.starts_with(e))
        {
            out.push_str(plain);
            rest = &rest[entity.len()..];
            continue;
        }
        let c = rest.chars().next().expect("rest is non-empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
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
/// Covers full names, three-letter macros, common abbreviations
/// (`Sept`/`Sep.` -> `sep`), and numeric months with or without a leading
/// zero (`6`/`06` -> `jun`), case-insensitively and ignoring a trailing dot.
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
        _ => v
            .parse::<usize>()
            .ok()
            .and_then(|n| MONTH_MACROS.get(n.wrapping_sub(1)).copied()),
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
///
/// Like `decode_html_entities`, an entity whose `&` is preceded by a
/// backslash is left alone to keep `sanitize_bibtex` idempotent.
fn decode_numeric_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("&#") {
        let escaped = if start > 0 {
            rest.as_bytes()[start - 1] == b'\\'
        } else {
            out.ends_with('\\')
        };
        if escaped {
            out.push_str(&rest[..start + 2]);
            rest = &rest[start + 2..];
            continue;
        }
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
        // Merged with the former api/openalex.rs copy (same shared function).
        assert_eq!(decode_html_entities("A &amp; B"), "A & B");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("it&#39;s"), "it's");
        assert_eq!(decode_html_entities("it&#x27;s"), "it's");
        assert_eq!(decode_html_entities("it&apos;s"), "it's");
        assert_eq!(decode_html_entities("&quot;hi&quot;"), "\"hi\"");
    }

    #[test]
    fn test_decode_html_entities_one_level_per_call() {
        // Double-encoded input loses exactly one level; decoded output is
        // never rescanned, so no new trigger patterns arise mid-pass.
        assert_eq!(decode_html_entities("&amp;amp;amp;"), "&amp;amp;");
        assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
        // Backslash-escaped ampersands never start an entity.
        assert_eq!(decode_html_entities(r"A \&amp; B"), r"A \&amp; B");
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
        assert_eq!(map_month("1"), Some("jan"));
        assert_eq!(map_month("6"), Some("jun"));
        assert_eq!(map_month("06"), Some("jun"));
        assert_eq!(map_month("12"), Some("dec"));
        assert_eq!(map_month("0"), None);
        assert_eq!(map_month("13"), None);
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

    #[test]
    fn test_sanitize_idempotent_on_double_encoded_input() {
        // One encoding level is stripped per pass, and the escaped output is
        // a fixed point: sanitize(sanitize(x)) == sanitize(x).
        let input = "@article{x2020,\n  title = {A &amp;amp;amp; B},\n  year = {2020}\n}";
        let once = sanitize_bibtex(input);
        assert!(once.text.contains(r"A \&amp;amp; B"), "got: {}", once.text);
        let twice = sanitize_bibtex(&once.text);
        assert_eq!(twice.text, once.text);
        assert!(!twice.changed, "second sanitize must be a no-op");
    }

    #[test]
    fn test_sanitize_idempotent_on_double_encoded_numeric_input() {
        let input = "@article{x2020,\n  title = {A &#38;#38; B},\n  year = {2020}\n}";
        let once = sanitize_bibtex(input);
        assert!(once.text.contains(r"A \&#38; B"), "got: {}", once.text);
        let twice = sanitize_bibtex(&once.text);
        assert_eq!(twice.text, once.text);
    }
}
