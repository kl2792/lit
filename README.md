# lit

Literature search CLI. Paste an arXiv ID, DOI, ISBN, or URL and get metadata, BibTeX, and PDFs.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the API surface, command rationale, output contracts, and integration patterns.
See [`docs/WORKFLOWS.md`](docs/WORKFLOWS.md) for worked examples of common tasks.

## Install

```
cargo build --release
cp target/release/lit /usr/local/bin/lit
```

Or: `make install`

Requires Rust 1.85+.

## Usage

### Auto-detect (just paste anything)

```
lit 2006.11239                        # arXiv lookup
lit 10.1145/3442188.3445899           # DOI lookup
lit https://arxiv.org/abs/2006.11239  # arXiv URL
lit https://doi.org/10.1145/...       # DOI URL
lit https://dblp.org/rec/...          # DBLP URL -> BibTeX
lit 978-0262039246                    # ISBN lookup
lit "attention is all you need"       # search
```

### Commands

```
lit search <query> [-l N] [--local | -s oa|ss|arxiv|cr|dblp|book|philpapers|clio|all]
                                 Search papers; remote APIs by default, --local
                                 restricts to downloaded papers
lit refs <id> [--hops N]         Get references of a paper
lit cites <id> [--hops N]        Get papers that cite this paper
lit path <a> <b> [--max-hops N]  Shortest citation path between two papers
lit download <id> [--source] [--url-only] [--dir DIR] [--citekey KEY]
                                 Download PDF (arXiv IDs and arXiv DOIs from
                                 arXiv; other DOIs via open access, Clio,
                                 EZProxy); --source for arXiv LaTeX source;
                                 --citekey names the output directory
lit read <id>...                 Locate paper text, one path per id in order;
                                 auto-downloads arXiv PDFs, DOIs, web pages and
                                 proceedings PDFs; a title or S2 id falls back to
                                 an open-access copy; a failed id is named on
                                 stderr, the rest still run
lit add <id> <bib_file> [--key KEY] [--force]
                                 Fetch BibTeX and upsert into file; --json
                                 prints {entry_key, bib_file, added}
lit misc <key> <bib_file> -t TITLE -y YEAR -a AUTHOR ... [--pdf PATH|URL] [--force]
                                 Append hand-rolled @misc entry; --pdf also
                                 ingests the artifact into etc/pdf/<key>/
lit attach <key> <bib_file> <pdf> [--force]
                                 Attach a PDF to an entry that already exists,
                                 leaving the BibTeX untouched
lit remove <key> <bib_file>      Remove an entry by citekey
lit verify <bib_file> [-j N] [--key KEY ...]
                                 Verify .bib entries against APIs, or only the
                                 named keys; --json prints [{key, status,
                                 detail}], status ok|mismatch|book|
                                 rate_limited|not_found
lit clean <bib_file> [--apply] [--prune] [--tex DIR ...]
                                 Scan for malformed entries, dupes, orphans,
                                 and LaTeX-breaking artifacts (&amp;, Unicode
                                 dashes, non-macro months)
lit check [--fix] [--conflicts] [--bib-file FILE]
                                 Check DB<->filesystem consistency
lit db stats|rebuild|path        Database operations; path prints every resolved
                                 state path and the source that set it
lit config [set NAME VALUE | unset NAME]
                                 List or change settings stored in the
                                 database: patient on|off, patient_budget SECS
lit clio auth                    Report EZProxy cookie status
lit clio sync [--check] [--force]
                                 Download and index the Columbia catalog
```

### Web pages

`lit read <URL>` and `lit add <URL> <bib_file>` accept page URLs on alignmentforum.org, lesswrong.com, greaterwrong.com, distill.pub and transformer-circuits.pub, and reject any other host that is not a proceedings host below (ADR-005).
The page text is stored once as `etc/pdf/<citekey>/paper.txt` with its URL, host, fetch time and extraction method in `source.yaml`, and later calls read it with no request.
`add` uses the page's DOI when it has one (Distill) and otherwise writes `@misc` with the site name as `howpublished` and the page `url`.
See [`docs/WORKFLOWS.md`](docs/WORKFLOWS.md) section 2c.

### Proceedings

`lit read <URL>` also accepts PDF and landing-page URLs on the open-access proceedings hosts of the rule table in `src/api/proceedings.rs`: NeurIPS (proceedings.neurips.cc, papers.nips.cc, papers.neurips.cc), PMLR, JMLR, OpenReview, ACL Anthology, AAAI, CVF, ECVA and IJCAI; a dl.acm.org URL is read as its DOI (ADR-010).
The PDF is stored as `etc/pdf/<citekey>/paper.pdf` with its `pdftotext` extraction, and `source.yaml` records the canonical PDF and landing URLs and the landing page's `citation_*` metadata.
A title or Semantic Scholar id that matches no local artifact is looked up on Semantic Scholar, then OpenAlex, for an arXiv id, DOI, open-access PDF or proceedings URL; the error names each source tried.
See [`docs/WORKFLOWS.md`](docs/WORKFLOWS.md) section 2d.

### Flags

```
-v, --verbose        Full details (default: concise one-line per result)
-b, --bib[=FILE]     Output BibTeX (append to FILE if given, stdout if not)
    --json           Machine-readable JSON output
    --no-cache       Bypass cache, fetch fresh
    --no-color       Disable colored output
    --patient        Wait out Semantic Scholar rate limits before falling back
    --patient-budget SECS
                     Patience budget per Semantic Scholar request (default
                     300); implies --patient
    --no-patient     Patient mode off, overriding the patient setting
-h, --help           Show help
```

### Patient mode

In patient mode (ADR-012) every Semantic Scholar request, whether from `search`, `add <title>`, `refs`, `cites`, `closure` or `path`, retries HTTP 429, 5xx and connection failures until its retry sleep would pass the patience budget, 300s by default; only then does the command fall back to OpenAlex or arXiv.
Waits follow the server's Retry-After, else 2, 4, 8, 16, 32 and then 60s, each plus up to 50% jitter, and each retry prints one stderr line such as `note: Semantic Scholar: rate limited (attempt 3), retrying in 8.4s (14s of 300s patience used)`.
Patient requests from every `lit` process sharing a database are spaced 1.1s apart through the file `<database>.s2-pacing`, matching the 1 request per second Semantic Scholar documents for an API key; a wait of 2s or more for a slot prints a line too.
Other hosts, and every request outside patient mode, keep the retry policy above.

```bash
lit search --patient "dropout Shapley value hidden units"
lit cites --patient-budget 600 2408.01416
lit config set patient on        # every invocation using this database is patient
lit search --no-patient "quick lookup"
```

### Search sources (`-s`)

| Flag | Source | Notes |
|------|--------|-------|
| `oa` | OpenAlex | Keyless requests share a daily budget per IP; set `OPENALEX_API_KEY` |
| `ss` | Semantic Scholar | |
| `arxiv` | arXiv API | Pairs of topic words, relevance sorted |
| `cr` | CrossRef | |
| `dblp` | DBLP | CS venue papers |
| `book` | OpenLibrary | Books |
| `philpapers` | PhilPapers | Philosophy |
| `clio` | Columbia Clio catalog | Local index; requires `lit clio sync` |
| `all` | All sources | Merge results |
| *(none)* | Cascade | SS -> OA + arXiv together -> CR -> books -> Clio; stops at the first tier with a result holding half the query's topic words in title, abstract or authors; such results rank first, in each backend's order (ADR-011) |

`--json` results carry `source`, the backend that returned each record.
A search that finds nothing while a source failed exits nonzero and names every source with its error (ADR-011).

### Environment

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_DB_PATH` | `etc/lit/lit.db` (relative to the binary) | SQLite database, which also holds the response cache |
| `LIT_PROJECT_ROOT` | nearest directory above the working directory holding a non-empty `etc/pdf/` | Root of the artifact library that `check`, `download` and `read` operate on |
| `LIT_CLIO_DB_PATH` | `etc/lit/clio.db` (nearest `etc/lit/` above the working directory) | Columbia catalog index |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `LIT_MAX_ATTEMPTS` | `5` | HTTP attempts before giving up; waits are the server's Retry-After, else 2s, 4s, 8s, 16s plus up to 50% jitter, at most 60s in total (ADR-004), 9s per source in the default search cascade (ADR-011); patient Semantic Scholar requests are limited by the patience budget alone unless this is set (ADR-012) |
| `NO_COLOR` | *(unset)* | Set to any non-empty value to disable color |
| `LIT_EMAIL` | `lit-cli@users.noreply.github.com` | Email for Unpaywall API |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids shared rate limits) |
| `OPENALEX_API_KEY` | *(unset)* | OpenAlex API key, sent as `Authorization: Bearer` (free; keyless requests share a per-IP daily budget) |
| `LIT_INLINE_TIMEOUT_MS` | `1000` | `lit-mcp` only: milliseconds a tool call may run before the server returns a task id and notifies on completion |

`lit db path` prints the resolved value of each path above together with the
source that set it.

## Examples

```
$ lit 2006.11239
Title: Denoising Diffusion Probabilistic Models
Authors: Jonathan Ho, Ajay Jain, Pieter Abbeel
Published: 2020-06-19
arXiv: 2006.11239
Categories: cs.LG, stat.ML
PDF: https://arxiv.org/pdf/2006.11239v2

Abstract: We present high quality image synthesis results...

@article{ho2020denoising,
  title = {Denoising Diffusion Probabilistic Models},
  author = {Jonathan Ho and Ajay Jain and Pieter Abbeel},
  year = {2020},
  eprint = {2006.11239},
  archivePrefix = {arXiv},
}
```

```
$ lit -b 2006.11239
@article{ho2020denoising,
  title = {Denoising Diffusion Probabilistic Models},
  ...
}
```

```
$ lit --json 2006.11239
{
  "title": "Denoising Diffusion Probabilistic Models",
  "authors": ["Jonathan Ho", "Ajay Jain", "Pieter Abbeel"],
  "year": "2020",
  ...
}
```

```
$ lit search "attention is all you need" -l 3
1. Vaswani 2025 | Attention Is All You Need | DOI:10.65215/2q58a426
2. Subakan 2021 | Attention Is All You Need In Speech Separation | DOI:10.1109/...
3. Choi 2020 | Channel Attention Is All You Need for Video Frame Interpolation | DOI:...
```

```
$ lit search -s dblp "attention is all you need"
1. 0009 2021 | Attentional Transfer is All You Need... |
...
```

```
$ lit verify refs.bib
Found 42 entries to verify
Verifying entries (parallel=4)...

  ! smith2020  [OpenAlex]: year:2020->2021
  x unknown2019: Some Paper Title

Total: 42 | OK: 40 (auto:39 manual:1) | Mismatch: 0 | Books: 1 | Not found: 1
```

## Caching

Responses are cached to disk with TTL:
- Search results: 24 hours
- DOI/arXiv/ISBN lookups: 7 days

Use `--no-cache` to bypass the cache and fetch fresh results.

## Testing

```
make test          # unit tests + bats integration tests
make test-unit     # cargo test
make test-bats     # bats test/lit.bats
```

### Sandboxed olmOCR

`docker/olmocr` runs image-only PDFs through olmOCR 2 on one NVIDIA GPU.
The runtime has no network, runs as a non-root user with no Linux capabilities,
uses a read-only root filesystem and input mount, and writes only to its output
mount.
The official image and embedded model are pinned by OCI digest; update the
digest deliberately when upgrading olmOCR.

Build the pinned image while network access is available:

```bash
docker compose -f docker/olmocr/compose.yaml build
```

Create an empty output directory owned by UID 65532, then process one PDF whose
basename contains only letters, digits, dots, underscores, or hyphens:

```bash
mkdir -p /path/to/output
sudo chown 65532:65532 /path/to/output
OLMOCR_INPUT_DIR=/path/to/pdfs \
OLMOCR_OUTPUT_DIR=/path/to/output \
OLMOCR_PDF=paper.pdf \
OLMOCR_GPU=0 \
docker compose -f docker/olmocr/compose.yaml run --rm olmocr
```

Treat generated Markdown as untrusted text and verify quotations against the
original page image.

### Artifact reconciliation

`lit check --fix` imports artifact metadata from `source.yaml` into the local
database.
When an artifact has `bibtex_key` provenance, pass the owning bibliography with
`--bib-file` so missing fields can be recovered from the BibTeX entry:

```
lit check --fix --bib-file refs.bib
lit check --fix --json --bib-file refs.bib
```

The repair is idempotent.
Artifacts with no unambiguous metadata are left unchanged and reported as
machine-readable `unresolved` records in JSON mode and written to
`.lit/unresolved-artifacts.json`.

### Attaching a PDF to an existing entry

`lit attach <citekey> <bib_file> <pdf> [--force]` files a PDF against a
bibliography entry that already exists.
It never creates or modifies a BibTeX entry, which is what separates it from
`lit misc --pdf`.

```
lit attach halpern2016actual refs.bib ~/Downloads/actual-causality.pdf
```

The source may be a local path or a URL.
`lit` rejects anything whose first bytes are not `%PDF`, writes
`etc/pdf/<citekey>/` with `paper.pdf`, `source.yaml` and extracted `paper.txt`,
and records in `source.yaml` the citekey, where the PDF came from, and the
entry's `doi` and `isbn` when it has them, so `lit check --fix` can reconcile
the artifact later.
An existing `etc/pdf/<citekey>/` directory is an error unless you pass
`--force`.

## Project structure

```
src/
  main.rs           CLI entry point (clap)
  lib.rs            Library surface shared by the CLI and the MCP server
  bin/lit-mcp.rs    MCP server binary
  mcp.rs            MCP tool definitions and handlers
  db.rs             SQLite store: papers, citations, response cache, FTS index
  detect.rs         Input type detection + normalization
  citekey.rs        BibTeX key generation (lastname2017word)
  http.rs           HTTP client (reqwest, retry + backoff, cache-aware, patient mode)
  http/pacing.rs    Semantic Scholar request slots shared across processes (ADR-012)
  format.rs         Colored output, truncation
  bibtex.rs         BibTeX parsing and generation
  sanitize.rs       BibTeX value normalization; `lit clean` derives its findings from it
  html.rs           Minimal HTML parser and plain-text renderer for web pages
  api/
    web.rs          Allowlisted web hosts (ADR-005): URL canonicalization, page metadata and body
    openalex.rs     OpenAlex API
    semantic_scholar.rs  Semantic Scholar API
    crossref.rs     CrossRef API
    dblp.rs         DBLP API
    arxiv.rs        arXiv API (XML)
    openlibrary.rs  OpenLibrary API
    philpapers.rs   PhilPapers API
    unpaywall.rs    Unpaywall API
    causalai.rs     causalai.net technical reports
    clio.rs         Columbia catalog: MARCXML sync, local index, EZProxy
  cmd/
    search.rs       Search with source selection + cascade
    refs.rs         Paper references
    cites.rs        Paper citations
    path.rs         Shortest citation path
    download.rs     PDF and arXiv source acquisition
    read.rs         Locate and extract paper text
    open.rs         Open in browser
    add.rs          Fetch + append BibTeX
    misc.rs         Hand-rolled @misc entries and PDF attachment
    clean.rs        Offline .bib linting
    verify.rs       Parallel .bib verification
    web.rs          Web-page artifacts for `read` and `add`
    check.rs        DB <-> filesystem reconciliation, rebuild
    config.rs       `lit config` settings and patient-mode resolution (ADR-012)
    clio.rs         Clio auth and sync
tests/              Integration tests keyed to the ADRs
test/
  lit.bats          Bats integration tests
  cache/            Cached API responses for offline testing
  web/              Trimmed Distill, Transformer Circuits and LessWrong samples
```
