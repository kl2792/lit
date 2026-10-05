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
lit search <query> [-l N] [--local | -s oa|ss|cr|dblp|book|philpapers|clio|all]
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
                                 auto-downloads arXiv PDFs; a failed id is
                                 named on stderr, the rest still run
lit add <id> <bib_file> [--key KEY] [--force]
                                 Fetch BibTeX and upsert into file
lit misc <key> <bib_file> -t TITLE -y YEAR -a AUTHOR ... [--pdf PATH|URL] [--force]
                                 Append hand-rolled @misc entry; --pdf also
                                 ingests the artifact into etc/pdf/<key>/
lit attach <key> <bib_file> <pdf> [--force]
                                 Attach a PDF to an entry that already exists,
                                 leaving the BibTeX untouched
lit remove <key> <bib_file>      Remove an entry by citekey
lit verify <bib_file> [-j N]     Verify .bib entries against APIs
lit clean <bib_file> [--apply] [--prune] [--tex DIR ...]
                                 Scan for malformed entries, dupes, orphans,
                                 and LaTeX-breaking artifacts (&amp;, Unicode
                                 dashes, non-macro months)
lit check [--fix] [--conflicts] [--bib-file FILE]
                                 Check DB<->filesystem consistency
lit db stats|rebuild|path        Database operations; path prints every resolved
                                 state path and the source that set it
lit clio auth                    Report EZProxy cookie status
lit clio sync [--check] [--force]
                                 Download and index the Columbia catalog
```

### Flags

```
-v, --verbose        Full details (default: concise one-line per result)
-b, --bib[=FILE]     Output BibTeX (append to FILE if given, stdout if not)
    --json           Machine-readable JSON output
    --no-cache       Bypass cache, fetch fresh
    --no-color       Disable colored output
-h, --help           Show help
```

### Search sources (`-s`)

| Flag | Source | Notes |
|------|--------|-------|
| `oa` | OpenAlex | Default primary |
| `ss` | Semantic Scholar | |
| `cr` | CrossRef | |
| `dblp` | DBLP | CS venue papers |
| `book` | OpenLibrary | Books |
| `philpapers` | PhilPapers | Philosophy |
| `clio` | Columbia Clio catalog | Local index; requires `lit clio sync` |
| `all` | All sources | Merge results |
| *(none)* | Cascade | OA -> SS -> CR -> books |

### Environment

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_DB_PATH` | `etc/lit/lit.db` (relative to the binary) | SQLite database, which also holds the response cache |
| `LIT_PROJECT_ROOT` | nearest directory above the working directory holding a non-empty `etc/pdf/` | Root of the artifact library that `check`, `download` and `read` operate on |
| `LIT_CLIO_DB_PATH` | `etc/lit/clio.db` (nearest `etc/lit/` above the working directory) | Columbia catalog index |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `LIT_MAX_ATTEMPTS` | `5` | HTTP attempts before giving up; waits are the server's Retry-After, else 2s, 4s, 8s, 16s plus up to 50% jitter, at most 60s in total (ADR-004) |
| `NO_COLOR` | *(unset)* | Set to any non-empty value to disable color |
| `LIT_EMAIL` | `lit-cli@users.noreply.github.com` | Email for Unpaywall API |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids shared rate limits) |
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
  http.rs           HTTP client (reqwest, retry + backoff, cache-aware)
  format.rs         Colored output, truncation
  bibtex.rs         BibTeX parsing and generation
  sanitize.rs       BibTeX value normalization; `lit clean` derives its findings from it
  api/
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
    check.rs        DB <-> filesystem reconciliation, rebuild
    clio.rs         Clio auth and sync
tests/              Integration tests keyed to the ADRs
test/
  lit.bats          Bats integration tests
  cache/            Cached API responses for offline testing
```
