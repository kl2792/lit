# Plan: Open Library URL support + cascade book-review filter

Updated 2026-05-19. Original plan (WorldCat JSON-LD) abandoned after empirical check: `worldcat.org/oclc/{id}.jsonld` returns Cloudflare 522 (dead). `search.worldcat.org/api/oclc/{id}` returns auth-gated HTML. OL Books/Works API is live, public, and has the 14 target books.

## Problem

`lit add` fails for pre-DOI books:
- Search cascade (SS → OA → CR → OL) stops early when SS returns a book review with title overlap ≥ 0.3. Tested: `lit add "Hays statistics for psychologists 1963"` → `Page_1964` (a journal review of the book, not the book).
- OL ISBN path already exists but ISBNs from training memory are often wrong or non-existent for pre-ISBN-era books.
- Result: 14 pre-DOI statistics books cannot be added with verified metadata.

## Solution: two additive changes

### Part 1 — Book-review false-positive filter (small, ~5 lines)

**Where:** `src/cmd/search.rs`, `fetch_cascade`.

**What:** Before accepting a result as "good enough" (score ≥ 0.3), check if its title matches a book-review pattern (case-insensitive: "book review", "review of", "reviews:"). If so, treat as score 0 and continue the cascade.

**Why:** SS indexes book review articles whose titles contain the book title. These score high on token overlap but are not the target. The cascade stops at SS instead of reaching OL.

**Test:** `lit add "Hays statistics for psychologists 1963"` should NOT return `Page_1964`.

---

### Part 2 — Open Library URL support (~100 lines)

**User workflow:**
```
# Find the book on openlibrary.org, copy URL (works or edition page)
lit add https://openlibrary.org/works/OL1153861W refs.bib
lit add https://openlibrary.org/books/OL13955598M refs.bib
```

#### 2a. `src/detect.rs`

Add `OpenLibraryUrl` variant to `InputType`. Pattern: `^https?://openlibrary\.org/(works|books)/`.

Supported URL patterns:
- `/works/OL{id}W` — work page (fetch work JSON + earliest edition for year/publisher + author)
- `/books/OL{id}M` — edition page (fetch edition JSON + author)

#### 2b. `src/api/openlibrary.rs` additions

```rust
/// Extract the OL entity type ("works"/"books") and ID from an OL URL.
pub fn parse_ol_url(url: &str) -> Option<(&str, &str)>  // → ("works", "OL1153861W")

/// URL for a works JSON record.
pub fn work_url(id: &str) -> String  // → "https://openlibrary.org/works/{id}.json"

/// URL for a books (edition) JSON record.
pub fn edition_url(id: &str) -> String  // → "https://openlibrary.org/books/{id}.json"

/// URL for the editions list of a work.
pub fn work_editions_url(id: &str) -> String  // → "https://openlibrary.org/works/{id}/editions.json?limit=50"

/// URL for an author JSON record.
pub fn author_url(key: &str) -> String  // key is e.g. "/authors/OL117058A"

/// Parse a works JSON response. Returns title + author keys (no publisher/year).
pub fn parse_work(body: &str) -> Result<WorkResult, Box<dyn std::error::Error>>

/// Parse a books (edition) JSON response. Returns title, publisher, year, author keys.
pub fn parse_edition(body: &str) -> Result<EditionResult, Box<dyn std::error::Error>>

/// Parse a works editions JSON response. Returns editions sorted by publish_date asc.
pub fn parse_editions_list(body: &str) -> Result<Vec<EditionResult>, Box<dyn std::error::Error>>

/// Parse an author JSON response. Returns display name.
pub fn parse_author(body: &str) -> Result<String, Box<dyn std::error::Error>>
```

New types (private to `openlibrary.rs`):
```rust
pub struct WorkResult { pub title: String, pub author_keys: Vec<String> }
pub struct EditionResult { pub title: String, pub publisher: Option<String>, pub year: String, pub author_keys: Vec<String> }
```

`parse_work` and `parse_edition` return only what's in the respective API response; the caller combines them.

#### 2c. `src/cmd/add.rs`

Add `InputType::OpenLibraryUrl` arm:

**Edition path** (`/books/OL{id}M`):
1. `parse_edition(edition_body)` → title, publisher, year, author_keys
2. Fetch first author key → `parse_author(author_body)` → name
3. Build `PaperResult` and call `generate_book_bibtex`

**Works path** (`/works/OL{id}W`):
1. `parse_work(work_body)` → title, author_keys
2. `parse_editions_list(editions_body)` → find earliest by year → publisher + year
3. Fetch first author key → `parse_author(author_body)` → name
4. Build `PaperResult` and call `generate_book_bibtex`

Use `tokio::join!` for parallel fetches where possible (work + editions, or work + author).

---

## Tests (write first — tests are the spec)

### `detect.rs` additions
```rust
detect_type("https://openlibrary.org/works/OL1153861W") → OpenLibraryUrl
detect_type("https://openlibrary.org/books/OL13955598M") → OpenLibraryUrl
detect_type("https://openlibrary.org/works/OL1153861W.json") → OpenLibraryUrl  // .json suffix ok
detect_type("http://openlibrary.org/books/OL13955598M") → OpenLibraryUrl  // http
```

### `openlibrary.rs` unit tests (fixture JSON, no network)
- `parse_ol_url`: works/books patterns + invalid URL → None
- `parse_work`: full fixture, missing authors, empty body
- `parse_edition`: full fixture, missing publisher, missing year, missing authors
- `parse_editions_list`: multi-edition fixture, sort by year, empty list
- `parse_author`: full fixture, missing name

### `search.rs` unit test
- book-review filter: result with title "Book Reviews: X" should score 0 and not stop cascade

### Integration smoke test (network, `#[ignore]` by default)
```
lit add https://openlibrary.org/works/OL1153861W /tmp/test.bib
# → @book{fisher1925statistical, title={Statistical methods for research workers}, author={Ronald Aylmer Fisher}, year={1925}, publisher={Oliver & Boyd}}
```

---

## Design checklist

- **KISS:** reuse `generate_book_bibtex()`, existing HTTP client — no new abstractions.
- **DRY:** parse logic in `openlibrary.rs`; `add.rs` only orchestrates.
- **Fail-fast:** 404/non-200 from OL → error with URL in message, not silent empty entry.
- **Graceful degradation:** each JSON field parsed independently; missing publisher → `generate_book_bibtex` will omit it (existing behavior).
- **Parallel fetches:** `tokio::join!` for parallel fetch of work + author (or work + editions).
- **Timeout:** inherits from existing HTTP client.
- **No auth:** OL API is public; no key required.
- **Author name**: use `personal_name` field if present, else `name`.

---

## Order of implementation

1. Write all tests (detect + parse + search filter) — they define the spec.
2. Implement Part 1 (book-review cascade filter) — 5 lines.
3. Implement Part 2a–2c (OL URL) — ~100 lines.
4. Run `make test` (Rust unit tests).
5. Smoke-test against Fisher 1925 and Cohen 1988.
6. `make push` to deploy updated `lit` binary.

---

## Pending book entries (CEV — unblocked after this plan)

### Books (OL URL workflow)
| Book | OL Works URL |
|---|---|
| Fisher 1925. Statistical Methods for Research Workers | https://openlibrary.org/works/OL1153861W |
| Fisher 1935. The Design of Experiments | search openlibrary |
| Ezekiel 1930. Methods of Correlation Analysis | search openlibrary |
| Theil 1961. Economic Forecasts and Policy | search openlibrary |
| Hays 1963. Statistics for Psychologists | search openlibrary |
| Scheffé 1959. The Analysis of Variance | search openlibrary |
| Searle 1971. Linear Models | search openlibrary |
| Searle 1987. Linear Models for Unbalanced Data | search openlibrary |
| Holland & Rubin 1983. Chapter in Wainer & Messick (eds.) | search openlibrary (book, not chapter) |
| McFadden 1974. Chapter in Zarembka (ed.) | search openlibrary (book, not chapter) |
| Lindeman, Merenda & Gold 1980 | search openlibrary |
| Cohen & Cohen 1975. Applied Multiple Regression | search openlibrary |

### Pre-DOI journal articles (lit misc fallback — no OL support)
| Article | Action |
|---|---|
| Wright 1921. J. Agric. Res. 20:557–585 | `lit misc` with verified metadata |
| Cochran 1934. Proc. Camb. Phil. Soc. | `lit misc` with verified metadata |
| Pearson 1905. Drapers' Memoirs | `lit misc` with verified metadata |
