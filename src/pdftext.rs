//! Extracting the text of a PDF, including PDFs that have no text to extract.
//!
//! A scanned paper carries page images and no text layer, so `pdftotext`
//! returns whitespace and the caller silently gets nothing. Every extraction
//! goes through [`extract`], which reports that outcome as [`Method::Scanned`]
//! rather than leaving the caller to discover an empty file. Recovering the
//! text costs a rasterise-and-recognise pass over every page, so it happens
//! only when the caller asks for it.

use std::path::Path;
use std::process::Command;

/// Characters of real text a page must average before we believe the text layer.
///
/// A born-digital page runs to thousands; a scan yields a handful of stray
/// marks from embedded stamps. Anything in between is a partial scan, which we
/// treat as a scan.
const MIN_CHARS_PER_PAGE: usize = 100;

/// Pages to rasterise at most, bounding OCR on a long scan.
const MAX_OCR_PAGES: usize = 400;

/// How the text of a PDF was obtained.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Method {
    /// Read from the PDF's own text layer.
    TextLayer,
    /// No usable text layer, and OCR was not requested: `txt` is near-empty.
    Scanned,
    /// Recognised from rasterised pages.
    Ocr,
}

/// Extract the text of `pdf` into `txt`, recognising the pages if asked.
///
/// Returns the method that produced the text. A PDF with no usable text layer
/// yields [`Method::Scanned`] when `recognise` is false: the near-empty output
/// still lands in `txt`, but the caller learns that it is near-empty instead
/// of discovering it later. With `recognise` set, a rasterise-and-recognise
/// pass replaces it, and a failure there is an error rather than a silent
/// fallback, because the caller asked for the text and did not get it.
pub fn extract(
    pdf: &Path,
    txt: &Path,
    recognise: bool,
) -> Result<Method, Box<dyn std::error::Error>> {
    run_pdftotext(pdf, txt)?;

    let pages = page_count(pdf);
    let text = std::fs::read_to_string(txt).unwrap_or_default();
    if !needs_ocr(&text, pages) {
        return Ok(Method::TextLayer);
    }
    if !recognise {
        return Ok(Method::Scanned);
    }

    let recognised = ocr(pdf)?;
    if needs_ocr(&recognised, pages) {
        return Err("OCR recovered no text from the pages".into());
    }
    std::fs::write(txt, recognised)?;
    Ok(Method::Ocr)
}

/// Whether `text` is too sparse to be a real text layer for `pages` pages.
///
/// `pages` of `None` means the page count is unknown, in which case one page
/// is assumed: that is the least forgiving reading, so a genuinely empty
/// extraction still trips the check.
pub fn needs_ocr(text: &str, pages: Option<usize>) -> bool {
    let real: usize = text.chars().filter(|c| !c.is_whitespace()).count();
    let pages = pages.unwrap_or(1).max(1);
    real / pages < MIN_CHARS_PER_PAGE
}

/// Pages in `pdf` according to `pdfinfo`, or `None` if it cannot be read.
fn page_count(pdf: &Path) -> Option<usize> {
    let out = Command::new("pdfinfo").arg(pdf).output().ok()?;
    parse_page_count(&String::from_utf8_lossy(&out.stdout))
}

/// The `Pages:` field of `pdfinfo` output.
fn parse_page_count(info: &str) -> Option<usize> {
    info.lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
}

/// Run `pdftotext -layout`, preserving the column geometry of a paper.
fn run_pdftotext(pdf: &Path, txt: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("pdftotext")
        .arg("-layout")
        .arg(pdf)
        .arg(txt)
        .status()?;
    if !status.success() {
        return Err("pdftotext failed".into());
    }
    Ok(())
}

/// Rasterise `pdf` and recognise every page with `tesseract`.
fn ocr(pdf: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let stem = dir.path().join("page");

    let status = Command::new("pdftoppm")
        .arg("-r")
        .arg("300")
        .arg("-png")
        .arg("-l")
        .arg(MAX_OCR_PAGES.to_string())
        .arg(pdf)
        .arg(&stem)
        .status()?;
    if !status.success() {
        return Err("pdftoppm failed".into());
    }

    let mut pages: Vec<_> = std::fs::read_dir(dir.path())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();
    if pages.is_empty() {
        return Err("pdftoppm produced no pages".into());
    }

    let mut text = String::new();
    for page in pages {
        let out = Command::new("tesseract").arg(&page).arg("stdout").output()?;
        if !out.status.success() {
            return Err("tesseract failed".into());
        }
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        text.push('\u{c}');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dense_text_layer_is_kept() {
        let page = "word ".repeat(200);
        assert!(!needs_ocr(&page, Some(1)));
    }

    #[test]
    fn an_empty_text_layer_asks_for_ocr() {
        assert!(needs_ocr("", Some(1)));
        assert!(needs_ocr("   \n\u{c}\n  ", Some(3)));
    }

    #[test]
    fn a_scan_stamp_does_not_count_as_a_text_layer() {
        // The 223 bytes poppler recovers from a pure Acrobat scan, spread over
        // a whole thesis, average to nothing.
        assert!(needs_ocr(&"x".repeat(223), Some(300)));
    }

    #[test]
    fn one_dense_page_does_not_vouch_for_a_scanned_book() {
        let cover = "word ".repeat(500);
        assert!(needs_ocr(&cover, Some(100)));
    }

    #[test]
    fn an_unknown_page_count_is_read_as_one_page() {
        assert!(!needs_ocr(&"word ".repeat(200), None));
        assert!(needs_ocr("short", None));
    }

    /// One page, no content stream: poppler reads it and finds no text.
    const BLANK_PDF: &[u8] = b"%PDF-1.4\n\
        1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
        2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
        3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>endobj\n\
        trailer<</Root 1 0 R>>\n";

    #[test]
    fn a_pdf_without_a_text_layer_reports_a_scan_when_ocr_was_not_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("blank.pdf");
        let txt = dir.path().join("blank.txt");
        std::fs::write(&pdf, BLANK_PDF).unwrap();

        assert_eq!(extract(&pdf, &txt, false).unwrap(), Method::Scanned);
        // The near-empty extraction is still written: the caller is told the
        // file is a scan, not left without a file.
        assert!(txt.exists());
    }

    #[test]
    fn page_count_comes_from_the_pages_field() {
        let info = "Title:          Steps\nPages:          27\nEncrypted:      no\n";
        assert_eq!(parse_page_count(info), Some(27));
        assert_eq!(parse_page_count("Encrypted:      no\n"), None);
        assert_eq!(parse_page_count("Pages:          many\n"), None);
    }
}
