/// `lit open <input>` -- Open a paper in the default browser.
///
/// Detects the input type and constructs the appropriate URL:
/// - Arxiv -> `https://arxiv.org/abs/{id}`
/// - DOI -> `https://doi.org/{doi}`
/// - DblpUrl / SemanticScholarUrl -> use as-is
/// - ISBN -> `https://openlibrary.org/isbn/{stripped}`
/// - Search -> `https://www.semanticscholar.org/search?q={encoded}`
///
/// Prints the URL to stderr, then opens with `open` (macOS) or `xdg-open` (Linux).
/// Falls back to printing the URL to stdout if neither is available.

use super::Context;
use crate::api::urlencode;
use crate::detect::{detect_type, normalize_arxiv, normalize_doi, normalize_isbn, InputType};
use crate::format;

pub fn run(_ctx: &Context, input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = match detect_type(input) {
        InputType::Arxiv => {
            let id = normalize_arxiv(input);
            format!("https://arxiv.org/abs/{}", id)
        }
        InputType::Doi => {
            let doi = normalize_doi(input);
            format!("https://doi.org/{}", doi)
        }
        InputType::DblpUrl | InputType::SemanticScholarUrl => input.to_string(),
        InputType::Isbn => {
            let stripped = normalize_isbn(input);
            format!("https://openlibrary.org/isbn/{}", stripped)
        }
        InputType::Search => {
            let encoded = urlencode(input);
            format!("https://www.semanticscholar.org/search?q={}", encoded)
        }
    };

    format::info(&format!("Opening: {}", url));

    // Try platform-specific openers, fall back to printing
    if try_open_browser(&url) {
        Ok(())
    } else {
        println!("{}", url);
        Ok(())
    }
}

/// Attempt to open URL with the platform browser opener.
/// Returns true if the command was found and launched (regardless of final result).
fn try_open_browser(url: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    };

    if let Ok(status) = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        if status.success() {
            return true;
        }
    }

    false
}
