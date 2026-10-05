/// BibTeX sanitization (ADR 001 change 1).
///
/// `bibtex::upsert_to_file` and `bibtex::append_to_file` run every entry
/// through `sanitize_bibtex` before writing, so `lit add`, `lit misc`, and
/// the global `-b/--bib` append path are covered by construction.
///
/// Transformations, per field value:
/// - HTML entities (named and numeric) decoded, then LaTeX-escaped:
///   `&` -> `\&`, `<`/`>` -> `\textless{}`/`\textgreater{}`.
/// - Unicode punctuation -> its LaTeX spelling, per the `PUNCTUATION` table:
///   dashes, curly quotes, ellipsis, prime, minus sign and non-breaking space.
/// - `month` field normalized to bare three-letter macros (`month = jun`);
///   unmappable values pass through unchanged with a warning.
///
/// Exemptions: `url` and `doi` fields are untouched; `\url{...}` spans inside
/// other fields are untouched. The pass is idempotent: each decoder strips
/// one encoding level per pass (mixed named+numeric encodings may strip one
/// level in each decoder within a single pass), and an ampersand preceded by
/// a backslash (the LaTeX-escaped output of a previous pass) never starts an
/// entity, so sanitized text is a fixed point.

use crate::bibtex::{parse_bib_file, BibEntry};

/// Unicode punctuation that pdfTeX cannot typeset from a plain source file,
/// paired with its LaTeX spelling.
///
/// Only punctuation appears here. An accented letter in an author name is
/// correct as written and a Greek letter in a title carries meaning, so
/// neither is translated; what breaks a build is the character that has a
/// LaTeX spelling and was emitted in its Unicode form instead.
///
/// Every replacement is ASCII and no ASCII character is a key, so applying
/// the table twice changes nothing. `test_punctuation_table_invariants` pins
/// both halves of that claim.
/// Three entries take the safe reading rather than the faithful one, because a
/// metadata provider emits these as encoding artifacts more often than as
/// typographic intent. U+00A0 becomes an ordinary space, not the `~` tie that
/// would make a scraped title unbreakable at every word. U+2011 loses its
/// non-breaking sense, and U+2212 renders at hyphen width rather than as the
/// `$-$` that would nest wrongly inside a title already in math mode.
const PUNCTUATION: [(char, &str); 12] = [
    ('\u{2010}', "-"),       // hyphen
    ('\u{2011}', "-"),       // non-breaking hyphen
    ('\u{2013}', "--"),      // en dash
    ('\u{2014}', "---"),     // em dash
    ('\u{2018}', "`"),       // left single quote
    ('\u{2019}', "'"),       // right single quote, the apostrophe case
    ('\u{201C}', "``"),      // left double quote
    ('\u{201D}', "''"),      // right double quote
    ('\u{2026}', "\\dots{}"), // horizontal ellipsis
    ('\u{2032}', "'"),       // prime
    ('\u{2212}', "-"),       // minus sign
    ('\u{00A0}', " "),       // non-breaking space
];

/// The `PUNCTUATION` characters present in `value`, in order of first
/// appearance and without repeats. Empty means only that this table has
/// nothing to do here; an HTML entity or a bare `&` still needs rewriting.
fn punctuation_findings(value: &str) -> Vec<char> {
    let mut found: Vec<char> = Vec::new();
    for c in value.chars() {
        if PUNCTUATION.iter().any(|(u, _)| *u == c) && !found.contains(&c) {
            found.push(c);
        }
    }
    found
}

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

/// Split a field value into segments, each paired with whether the pass leaves
/// it verbatim. `\url{...}` spans are verbatim; everything else is transformed.
///
/// `transform_value` rewrites exactly the non-verbatim segments and
/// `field_findings` inspects exactly the same ones, so the writer and the
/// linter cannot disagree about what is in scope.
fn segments(value: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(idx) = rest.find("\\url{") {
        out.push((&rest[..idx], false));
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
        out.push((&rest[idx..pos], true));
        rest = &rest[pos..];
    }
    out.push((rest, false));
    out
}

/// Transform a field value, leaving `\url{...}` spans verbatim.
fn transform_value(value: &str) -> String {
    segments(value)
        .into_iter()
        .map(|(seg, verbatim)| {
            if verbatim {
                seg.to_string()
            } else {
                transform_segment(seg)
            }
        })
        .collect()
}

/// Why `lit clean` should rewrite this field, or empty if it should not.
///
/// The first test is the one that matters: a field the pass would leave
/// unchanged yields no findings, whatever it contains. So every finding names
/// something `lit clean --apply` actually fixes, and a second run reports
/// nothing. A linter that scanned for defects independently would flag text
/// the pass is designed to preserve, such as the `\&amp;` that
/// `decode_html_entities` deliberately leaves alone, and never converge.
///
/// The remaining work is explanatory: naming which characters are responsible.
pub fn field_findings(name: &str, value: &str) -> Vec<String> {
    if name == "url" || name == "doi" {
        return Vec::new();
    }
    if name == "month" {
        // An unmappable value is left in place by design, so reporting it
        // would be a finding no run can ever clear.
        return match (map_month(value), is_month_macro(value)) {
            (Some(_), false) => vec![format!("non-macro month value '{}'", value)],
            _ => Vec::new(),
        };
    }
    if transform_value(value) == value {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for (seg, verbatim) in segments(value) {
        if verbatim {
            continue;
        }
        for c in punctuation_findings(seg) {
            let latex = PUNCTUATION
                .iter()
                .find(|(u, _)| *u == c)
                .map(|(_, l)| *l)
                .unwrap_or("");
            out.push(format!("U+{:04X} should be written {}", c as u32, latex));
        }
        let decoded = decode_numeric_entities(&decode_html_entities(seg));
        if decoded != seg {
            out.push("HTML entity".to_string());
        }
        if strip_html_tags(&decoded) != decoded {
            out.push("HTML tag".to_string());
        }
    }
    out.dedup();
    if out.is_empty() {
        out.push("character needing a LaTeX escape".to_string());
    }
    out.into_iter().map(|f| format!("{} in {}", f, name)).collect()
}

/// Decode entities, normalize dashes, then LaTeX-escape.
fn transform_segment(s: &str) -> String {
    let decoded = decode_numeric_entities(&decode_html_entities(s));
    latex_escape(&normalize_punctuation(&strip_html_tags(&decoded)))
}

/// Remove markup tags that may appear when a metadata provider HTML-escapes
/// an otherwise plain title (for example `&lt;i&gt;L&lt;/i&gt;`).
/// Comparisons such as `x < y` are preserved because the `<` is not followed
/// by a tag-shaped character.
fn strip_html_tags(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic()
                || bytes[i + 1] == b'/'
                || bytes[i + 1] == b'!'
                || bytes[i + 1] == b'?')
            && let Some(end) = s[i + 1..].find('>')
        {
            i += end + 2;
            continue;
        }
        let c = s[i..].chars().next().expect("valid UTF-8 boundary");
        out.push(c);
        i += c.len_utf8();
    }
    out
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

/// Rewrite every Unicode punctuation character in `PUNCTUATION` to its LaTeX
/// spelling, in a single pass so that a replacement is never itself rescanned.
fn normalize_punctuation(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match PUNCTUATION.iter().find(|(u, _)| *u == c) {
            Some((_, latex)) => out.push_str(latex),
            None => out.push(c),
        }
    }
    out
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
    fn test_strip_metadata_html_tags_but_preserve_comparisons() {
        assert_eq!(transform_segment("&lt;i&gt;L&lt;/i&gt; = λ W"), "L = λ W");
        assert_eq!(transform_segment("x < y"), "x \\textless{} y");
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
    fn test_normalize_punctuation() {
        // The case this table was written for: the DOI record for Bareinboim
        // et al. (2022) carries a curly apostrophe in "On Pearl's Hierarchy".
        assert_eq!(transform_segment("On Pearl\u{2019}s Hierarchy"), "On Pearl's Hierarchy");
        assert_eq!(transform_segment("\u{201C}quoted\u{201D}"), "``quoted''");
        assert_eq!(transform_segment("a \u{2018}b\u{2019} c"), "a `b' c");
        assert_eq!(transform_segment("1\u{2013}9"), "1--9");
        assert_eq!(transform_segment("a \u{2014} b"), "a --- b");
        assert_eq!(transform_segment("and so on\u{2026}"), "and so on\\dots{}");
        assert_eq!(transform_segment("Lee\u{00A0}et al."), "Lee et al.");
        assert_eq!(transform_segment("non\u{2010}linear"), "non-linear");
        assert_eq!(transform_segment("non\u{2011}linear"), "non-linear");
        assert_eq!(transform_segment("x\u{2032}"), "x'");
        assert_eq!(transform_segment("\u{2212}1"), "-1");
    }

    #[test]
    fn test_normalize_punctuation_preserves_letters() {
        // Accented and Greek letters are correct as written; only punctuation
        // has a LaTeX spelling that the source was supposed to use.
        assert_eq!(transform_segment("Bareinboim, Pl\u{e8}cko"), "Bareinboim, Pl\u{e8}cko");
        assert_eq!(transform_segment("L = \u{3bb} W"), "L = \u{3bb} W");
    }

    #[test]
    fn test_normalize_punctuation_is_idempotent() {
        // Every replacement is ASCII and no ASCII character is a table key.
        let once = transform_segment("On Pearl\u{2019}s \u{201C}Hierarchy\u{201D}, 1\u{2013}9");
        assert_eq!(transform_segment(&once), once);
    }

    #[test]
    fn test_punctuation_findings() {
        assert_eq!(punctuation_findings("On Pearl\u{2019}s"), vec!['\u{2019}']);
        // Ordered by first appearance, without repeats.
        assert_eq!(
            punctuation_findings("a\u{2013}b \u{2019} c\u{2013}d"),
            vec!['\u{2013}', '\u{2019}']
        );
        assert!(punctuation_findings("plain ascii, 1--9").is_empty());
        assert!(punctuation_findings("Pl\u{e8}cko").is_empty());
    }

    #[test]
    fn test_punctuation_table_invariants() {
        // The idempotence argument in the table's doc comment, as a test: every
        // key is non-ASCII, so no replacement can re-enter the table, and no
        // replacement contains a character `latex_escape` would rewrite.
        for (key, latex) in PUNCTUATION {
            assert!(!key.is_ascii(), "key {:?} is ASCII", key);
            assert_eq!(normalize_punctuation(latex), latex, "replacement {:?} re-fires", latex);
            assert_eq!(latex_escape(latex), latex, "replacement {:?} needs escaping", latex);
        }
    }

    #[test]
    fn test_findings_are_empty_exactly_when_the_pass_is_a_no_op() {
        // The convergence contract: `lit clean` must not report what
        // `--apply` will not change, or it repeats the finding forever.
        let cases = [
            ("title", "On Pearl\u{2019}s Hierarchy"),
            ("title", "A &amp; B"),
            ("title", r"A \&amp; B"),        // an intended fixed point
            ("title", "A \\& B"),
            ("note", "see \\url{http://x.com/a\u{2013}b}"), // verbatim span
            ("url", "http://x.com/a\u{2013}b"),
            ("doi", "10.1/a\u{2013}b"),
            ("month", "June"),
            ("month", "jun"),
            ("month", "June 2020"),          // unmappable, so unfixable
            ("pages", "507--556"),
        ];
        for (name, value) in cases {
            // Mirrors `sanitize_entry_fields`: exempt fields are never passed
            // to the transform at all, and month takes the macro branch.
            let sanitized = match name {
                "url" | "doi" => value.to_string(),
                "month" => map_month(value).unwrap_or(value).to_string(),
                _ => transform_value(value),
            };
            let findings = field_findings(name, value);
            assert_eq!(
                findings.is_empty(),
                sanitized == value,
                "{} = {:?}: findings {:?}, pass gives {:?}",
                name, value, findings, sanitized
            );
        }
    }

    #[test]
    fn test_findings_name_the_character() {
        assert_eq!(
            field_findings("title", "On Pearl\u{2019}s"),
            vec!["U+2019 should be written ' in title"]
        );
        assert_eq!(field_findings("title", "A &amp; B"), vec!["HTML entity in title"]);
        assert_eq!(field_findings("title", "<i>L</i>"), vec!["HTML tag in title"]);
        assert_eq!(
            field_findings("title", "Smith & Jones"),
            vec!["character needing a LaTeX escape in title"]
        );
    }

    #[test]
    fn test_url_spans_and_exempt_fields_keep_their_punctuation() {
        // The pass copies `\url{...}` verbatim, so the lint must not ask for a
        // rewrite there either.
        let value = "see \\url{http://x.com/a\u{2013}b} and 1\u{2013}9";
        assert_eq!(
            transform_value(value),
            "see \\url{http://x.com/a\u{2013}b} and 1--9"
        );
        assert_eq!(
            field_findings("note", value),
            vec!["U+2013 should be written -- in note"]
        );
    }

    #[test]
    fn test_numeric_entity_decoding_into_a_table_character() {
        // The only place the decode-then-normalize order in `transform_segment`
        // is observable: the entity must decode first, then normalize.
        assert_eq!(transform_segment("1&#8211;9"), "1--9");
        assert_eq!(transform_segment("Pearl&#8217;s"), "Pearl's");
    }

    #[test]
    fn test_sanitize_bibtex_rewrites_curly_apostrophe() {
        let input = "@inbook{bareinboim2022pearls,\n  title = {On Pearl\u{2019}s Hierarchy},\n  year = {2022}\n}";
        let out = sanitize_bibtex(input);
        assert!(out.changed);
        assert!(out.text.contains("On Pearl's Hierarchy"), "got: {}", out.text);
        assert!(!out.text.contains('\u{2019}'));
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
