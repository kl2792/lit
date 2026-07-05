/// CausalAI Lab technical reports (causalai.net/rNNN).
///
/// These reports have no DOI or arXiv id and the site's own `references/rNNN.bib`
/// files are frequently missing (404) or malformed, so lit derives a clean entry
/// from the report's title page instead. The page follows a fixed template:
///
/// ```text
/// TECHNICAL REPORT
/// R-145
/// Mar, 2026                 (or "First version: .../Last version: ...")
///
/// <title, one or two lines>
///
/// <authors>
/// <affiliation / email>     (optional)
/// Abstract ...
/// ```
///
/// `parse_header` turns the `pdftotext` rendering of that page into structured
/// metadata; the caller builds the BibTeX entry and ingests the PDF through the
/// shared `misc` artifact pipeline.

use std::sync::LazyLock;

use regex::Regex;

static RNUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^r-\s*(\d+)\b").unwrap());
static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b((?:19|20)\d{2})\b").unwrap());
static AFFIL_DIGIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z]\s+\d").unwrap());
static DIGIT_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());

/// Structured metadata parsed from a report's title page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalaiMeta {
    /// Report number, e.g. `R-145`.
    pub number: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: String,
}

/// PDF URL for a canonical report id (e.g. `r145`).
pub fn pdf_url(id: &str) -> String {
    format!("https://causalai.net/{}.pdf", id)
}

/// Download a report PDF and parse its title page. Returns the metadata and the
/// raw PDF bytes (so callers can ingest the artifact without a second fetch).
pub(crate) fn fetch(id: &str) -> Result<(CausalaiMeta, Vec<u8>), Box<dyn std::error::Error>> {
    let url = pdf_url(id);
    let bytes = crate::cmd::misc::fetch_pdf_bytes_via_curl(&url).map_err(|e| {
        format!("{}\nhint: download in a browser and use `lit misc --pdf <path>`", e)
    })?;
    if !bytes.starts_with(b"%PDF") {
        return Err(format!("{} did not return a PDF", url).into());
    }
    let meta = parse_header(&first_page_text(&bytes)?)?;
    Ok((meta, bytes))
}

/// Render the first page of an in-memory PDF as text via `pdftotext -layout`.
fn first_page_text(bytes: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join(format!("lit_causalai_{}.pdf", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    let out = std::process::Command::new("pdftotext")
        .args(["-layout", "-f", "1", "-l", "1"])
        .arg(&tmp)
        .arg("-")
        .output();
    let _ = std::fs::remove_file(&tmp);
    let out = out.map_err(|e| format!("failed to run pdftotext (is poppler installed?): {}", e))?;
    if !out.status.success() {
        return Err("pdftotext failed on the report PDF".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Build a `PaperResult` (for `lit <url>` display) from parsed metadata.
pub(crate) fn to_paper_result(meta: &CausalaiMeta, id: &str) -> super::PaperResult {
    super::PaperResult {
        title: meta.title.clone(),
        authors: meta.authors.clone(),
        year: meta.year.clone(),
        doi: None,
        arxiv_id: None,
        citations: None,
        venue: Some(format!(
            "Technical Report {}, Causal Artificial Intelligence Lab, Columbia University",
            meta.number
        )),
        pdf_url: Some(pdf_url(id)),
        abstract_text: None,
        isbn: None,
        s2_id: None,
        published_date: None,
        categories: vec![],
    }
}

/// Whether a title-page line is the authors line (vs. a title or affiliation line).
///
/// Authors are joined with " and " (`Maiti and Jain and Bareinboim`) or carry
/// affiliation superscripts (`Maiti 1 Bareinboim 1`). Title lines have neither.
fn looks_like_authors(line: &str) -> bool {
    line.contains(" and ") || AFFIL_DIGIT_RE.is_match(line)
}

/// Whether a line begins the abstract or an affiliation block, marking the end
/// of the title/author region.
fn is_terminator(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.starts_with("abstract")
        || l.contains("university")
        || l.contains("laborator")
        || l.contains("@")
        || l.starts_with("{")
}

/// Split an authors line into individual "First Last" names.
fn parse_authors(line: &str) -> Vec<String> {
    let parts: Vec<String> = if line.contains(" and ") {
        line.split(" and ").map(|s| s.to_string()).collect()
    } else {
        // Affiliation digits delimit the authors (e.g. "Maiti 1 Bareinboim 1").
        DIGIT_SPLIT_RE.split(line).map(|s| s.to_string()).collect()
    };
    parts
        .iter()
        .map(|p| normalize_author(p))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Strip digits and stray punctuation from a single author token, collapse
/// internal whitespace.
fn normalize_author(s: &str) -> String {
    let no_digits: String = s.chars().filter(|c| !c.is_ascii_digit()).collect();
    no_digits
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == ',' || c == '.' || c == ';')
        .trim()
        .to_string()
}

/// Parse a CausalAI report title page (as rendered by `pdftotext`) into metadata.
pub fn parse_header(text: &str) -> Result<CausalaiMeta, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    // 1. Report number.
    let (idx_r, number) = lines
        .iter()
        .enumerate()
        .find_map(|(i, l)| RNUM_RE.captures(l).map(|c| (i, format!("R-{}", &c[1]))))
        .ok_or("no R-NNN report number found on title page")?;

    // 2. Date block: consecutive lines bearing a year. Use the latest year
    //    (handles "First version: 2025 / Last version: 2025").
    let mut idx_title = idx_r + 1;
    let mut year: Option<u32> = None;
    while idx_title < lines.len() {
        let years: Vec<u32> = YEAR_RE
            .captures_iter(lines[idx_title])
            .filter_map(|c| c[1].parse().ok())
            .collect();
        if years.is_empty() {
            break;
        }
        year = Some(year.unwrap_or(0).max(years.into_iter().max().unwrap()));
        idx_title += 1;
    }
    let year = year.ok_or("no publication year found on title page")?.to_string();

    // 3. Title runs from idx_title up to the authors line; bail at the abstract
    //    or affiliation block.
    let mut authors_idx = None;
    for i in idx_title..lines.len() {
        if i > idx_title && is_terminator(lines[i]) {
            break;
        }
        if i > idx_title && looks_like_authors(lines[i]) {
            authors_idx = Some(i);
            break;
        }
    }
    let authors_idx = authors_idx.ok_or("could not locate the authors line on title page")?;

    let title = lines[idx_title..authors_idx].join(" ").trim().to_string();
    if title.is_empty() {
        return Err("empty title parsed from title page".into());
    }
    let authors = parse_authors(lines[authors_idx]);
    if authors.is_empty() {
        return Err("no authors parsed from title page".into());
    }

    Ok(CausalaiMeta { number, title, authors, year })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `pdftotext -layout` title pages (whitespace trimmed per line).
    const R145: &str = "TECHNICAL REPORT
R-145
Mar, 2026

Sequential Causal Games

Aurghya Maiti 1 Elias Bareinboim 1

Abstract   One of the earliest formal responses to this problem";

    const R125: &str = "TECHNICAL REPORT
R-125
First version: Jan, 2025
Last version: July, 2025

Counterfactual Rationality:
A Causal Approach to Game Theory

Aurghya Maiti and Prateek Jain and Elias Bareinboim
Causal Artificial Intelligence Laboratory
Columbia University, USA
{aurghya,eb}@cs.columbia.edu

Abstract";

    const R152: &str = "TECHNICAL REPORT
R-152
May, 2026

Learning in Causal Markov Games

Aurghya Maiti and Elias Bareinboim
Causal Artificial Intelligence Lab
Columbia University
{aurghya,eb}@columbia.edu

Abstract";

    #[test]
    fn parse_r145_two_authors_digit_delimited() {
        let m = parse_header(R145).unwrap();
        assert_eq!(m.number, "R-145");
        assert_eq!(m.title, "Sequential Causal Games");
        assert_eq!(m.authors, vec!["Aurghya Maiti", "Elias Bareinboim"]);
        assert_eq!(m.year, "2026");
    }

    #[test]
    fn parse_r125_multiline_title_three_authors() {
        let m = parse_header(R125).unwrap();
        assert_eq!(m.number, "R-125");
        assert_eq!(m.title, "Counterfactual Rationality: A Causal Approach to Game Theory");
        assert_eq!(m.authors, vec!["Aurghya Maiti", "Prateek Jain", "Elias Bareinboim"]);
        assert_eq!(m.year, "2025"); // latest of the two version years
    }

    #[test]
    fn parse_r152_single_title_two_authors() {
        let m = parse_header(R152).unwrap();
        assert_eq!(m.number, "R-152");
        assert_eq!(m.title, "Learning in Causal Markov Games");
        assert_eq!(m.authors, vec!["Aurghya Maiti", "Elias Bareinboim"]);
        assert_eq!(m.year, "2026");
    }

    #[test]
    fn pdf_url_format() {
        assert_eq!(pdf_url("r145"), "https://causalai.net/r145.pdf");
    }

    #[test]
    fn missing_report_number_errors() {
        assert!(parse_header("Some Random Paper\nby Someone\n2020").is_err());
    }
}
