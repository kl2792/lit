/// BibTeX parsing and generation.
///
/// Handles parsing `.bib` files into structured entries, extracting BibTeX
/// blocks from mixed output, and appending entries to files.

use std::fs;
use std::path::Path;

/// A single BibTeX entry.
#[derive(Debug, Clone)]
pub struct BibEntry {
    /// Entry type: article, book, inproceedings, etc.
    pub entry_type: String,
    /// Citation key: lastname2017word
    pub key: String,
    /// Ordered list of (field_name, field_value) pairs.
    pub fields: Vec<(String, String)>,
}

impl std::fmt::Display for BibEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}{{{},\n", self.entry_type, self.key)?;
        for (i, (name, value)) in self.fields.iter().enumerate() {
            if i < self.fields.len() - 1 {
                write!(f, "  {} = {{{}}},\n", name, value)?;
            } else {
                // Last field: no trailing comma
                write!(f, "  {} = {{{}}}\n", name, value)?;
            }
        }
        write!(f, "}}")
    }
}

impl BibEntry {
    /// Get the value of a field by name (case-insensitive).
    pub fn get_field(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_lowercase();
        self.fields
            .iter()
            .find(|(n, _)| n.to_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }

    /// Check if this entry has a `% lit:skip` marker in its preceding comments.
    /// This is tracked externally during parsing, not stored in the entry itself.
    pub fn has_skip_marker(content: &str, entry_start: usize) -> bool {
        // Look at up to 3 lines before the entry start
        let before = &content[..entry_start];
        let lines: Vec<&str> = before.lines().collect();
        let start = if lines.len() > 3 { lines.len() - 3 } else { 0 };
        lines[start..].iter().any(|line| line.contains("% lit:skip"))
    }
}

/// Parse a `.bib` file content into entries.
///
/// Handles:
/// - `@type{key, field = {value}, ...}`
/// - `@type{key, field = "value", ...}`
/// - Multi-line values
/// - Nested braces in values
pub fn parse_bib_file(content: &str) -> Vec<BibEntry> {
    let mut entries = Vec::new();
    let mut pos = 0;
    let bytes = content.as_bytes();

    while pos < bytes.len() {
        // Find next @
        match content[pos..].find('@') {
            Some(offset) => pos += offset,
            None => break,
        }

        let entry_start = pos;
        pos += 1; // skip '@'

        // Read entry type
        let type_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let entry_type = content[type_start..pos].to_lowercase();

        // Skip to opening brace
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        pos += 1; // skip '{'

        // Skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Read key (everything until comma or whitespace)
        let key_start = pos;
        while pos < bytes.len() && bytes[pos] != b',' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let key = content[key_start..pos].trim().to_string();

        // Skip to after the comma (or stop at closing brace if no fields)
        while pos < bytes.len() && bytes[pos] != b',' && bytes[pos] != b'}' {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b',' {
            pos += 1; // skip ','
        }

        // Parse fields until we hit the closing brace at depth 0
        let mut fields = Vec::new();
        let mut brace_depth: i32 = 1; // We're inside the entry's opening brace

        loop {
            // Skip whitespace
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos >= bytes.len() {
                break;
            }

            // Check for closing brace
            if bytes[pos] == b'}' {
                brace_depth -= 1;
                pos += 1;
                if brace_depth <= 0 {
                    break;
                }
                continue;
            }

            // Try to read a field: name = value
            let field_start = pos;
            // Read field name
            while pos < bytes.len()
                && bytes[pos] != b'='
                && bytes[pos] != b'}'
                && !bytes[pos].is_ascii_whitespace()
            {
                pos += 1;
            }
            let field_name = content[field_start..pos].trim().to_lowercase();

            // Skip whitespace
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }

            if pos >= bytes.len() || bytes[pos] == b'}' {
                // No '=' found, this is the closing brace
                if pos < bytes.len() && bytes[pos] == b'}' {
                    pos += 1;
                }
                break;
            }

            if bytes[pos] != b'=' {
                // Skip unexpected content
                pos += 1;
                continue;
            }
            pos += 1; // skip '='

            // Skip whitespace
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }

            if pos >= bytes.len() {
                break;
            }

            // Read value: either {braced} or "quoted" or bare
            let value = if bytes[pos] == b'{' {
                pos += 1; // skip opening brace
                let val_start = pos;
                let mut depth = 1;
                while pos < bytes.len() && depth > 0 {
                    if bytes[pos] == b'{' {
                        depth += 1;
                    } else if bytes[pos] == b'}' {
                        depth -= 1;
                    }
                    if depth > 0 {
                        pos += 1;
                    }
                }
                let val = content[val_start..pos].to_string();
                if pos < bytes.len() {
                    pos += 1; // skip closing brace
                }
                val
            } else if bytes[pos] == b'"' {
                pos += 1; // skip opening quote
                let val_start = pos;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 1; // skip backslash
                        if pos < bytes.len() {
                            pos += 1; // skip escaped char
                        }
                    } else {
                        pos += 1;
                    }
                }
                let val = content[val_start..pos].to_string();
                if pos < bytes.len() {
                    pos += 1; // skip closing quote
                }
                val
            } else {
                // Bare value (number or macro)
                let val_start = pos;
                while pos < bytes.len()
                    && bytes[pos] != b','
                    && bytes[pos] != b'}'
                    && !bytes[pos].is_ascii_whitespace()
                {
                    pos += 1;
                }
                content[val_start..pos].trim().to_string()
            };

            if !field_name.is_empty() {
                fields.push((field_name, value));
            }

            // Skip whitespace and optional comma
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] == b',' {
                pos += 1;
            }
        }

        // Skip comment-only entries like @comment or @preamble
        if entry_type == "comment" || entry_type == "preamble" || entry_type == "string" {
            continue;
        }

        let skip = BibEntry::has_skip_marker(content, entry_start);
        if !skip {
            entries.push(BibEntry {
                entry_type,
                key,
                fields,
            });
        }
    }

    entries
}

/// Append a BibTeX entry string to a file.
///
/// Adds a blank line separator before the entry if the file already exists
/// and is non-empty.
pub fn append_to_file(path: &Path, entry: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    // Add a blank line separator if file is non-empty
    let metadata = fs::metadata(path)?;
    if metadata.len() > 0 {
        writeln!(file)?;
    }

    writeln!(file, "{}", entry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_entry() {
        let bib = r#"@article{ho2020denoising,
  title = {Denoising Diffusion Probabilistic Models},
  author = {Jonathan Ho and Ajay Jain and Pieter Abbeel},
  year = {2020},
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "article");
        assert_eq!(entries[0].key, "ho2020denoising");
        assert_eq!(entries[0].get_field("title"), Some("Denoising Diffusion Probabilistic Models"));
        assert_eq!(entries[0].get_field("year"), Some("2020"));
    }

    #[test]
    fn parse_quoted_values() {
        let bib = r#"@article{test2021,
  title = "Some Title",
  year = "2021",
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_field("title"), Some("Some Title"));
    }

    #[test]
    fn parse_nested_braces() {
        let bib = r#"@article{test2021,
  title = {A {Deep} Learning Approach},
  year = {2021},
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_field("title"), Some("A {Deep} Learning Approach"));
    }

    #[test]
    fn parse_multiple_entries() {
        let bib = r#"@article{first2020,
  title = {First Paper},
  year = {2020},
}

@inproceedings{second2021,
  title = {Second Paper},
  year = {2021},
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "first2020");
        assert_eq!(entries[1].key, "second2021");
        assert_eq!(entries[1].entry_type, "inproceedings");
    }

    #[test]
    fn parse_multiline_value() {
        let bib = r#"@article{test2021,
  title = {A Very Long Title
    That Spans Multiple Lines},
  year = {2021},
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        let title = entries[0].get_field("title").unwrap();
        assert!(title.contains("A Very Long Title"));
        assert!(title.contains("That Spans Multiple Lines"));
    }

    #[test]
    fn parse_skip_lit_skip() {
        let bib = r#"% lit:skip
@article{skipped2021,
  title = {Should Be Skipped},
  year = {2021},
}

@article{kept2021,
  title = {Should Be Kept},
  year = {2021},
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "kept2021");
    }

    #[test]
    fn parse_no_trailing_comma() {
        let bib = r#"@article{test2021,
  title = {Test Paper},
  year = {2021}
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get_field("year"), Some("2021"));
    }

    #[test]
    fn entry_to_string() {
        let entry = BibEntry {
            entry_type: "article".to_string(),
            key: "ho2020denoising".to_string(),
            fields: vec![
                ("title".to_string(), "Denoising Diffusion".to_string()),
                ("author".to_string(), "Jonathan Ho".to_string()),
                ("year".to_string(), "2020".to_string()),
            ],
        };
        let s = entry.to_string();
        assert!(s.starts_with("@article{ho2020denoising,"));
        assert!(s.contains("title = {Denoising Diffusion},"));
        assert!(s.contains("year = {2020}"));
        assert!(s.ends_with('}'));
    }

    #[test]
    fn append_to_new_file() {
        let dir = std::env::temp_dir().join("lit_bibtex_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.bib");

        let entry = "@article{test2021,\n  title = {Test},\n  year = {2021}\n}";
        append_to_file(&path, entry).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("@article{test2021"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_book_entry() {
        let bib = r#"@book{pearl2009causality,
  title = {Causality},
  author = {Judea Pearl},
  year = {2009},
  publisher = {Cambridge University Press},
  isbn = {9780521895606}
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "book");
        assert_eq!(entries[0].get_field("publisher"), Some("Cambridge University Press"));
        assert_eq!(entries[0].get_field("isbn"), Some("9780521895606"));
    }

    #[test]
    fn parse_escaped_quotes() {
        let bib = r#"@article{test2021,
  title = "A so-called \"great\" idea",
  year = "2021",
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get_field("title"),
            Some(r#"A so-called \"great\" idea"#)
        );
    }

    #[test]
    fn parse_skips_comment_entries() {
        let bib = r#"@comment{This is a comment}
@article{real2021,
  title = {Real Entry},
  year = {2021}
}"#;
        let entries = parse_bib_file(bib);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "real2021");
    }
}
