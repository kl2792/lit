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
lit search <query> [-l N] [-s oa|ss|cr|dblp|book|philpapers|all] [--remote]
                                 Search papers; local DB by default
lit refs <id> [--hops N]         Get references of a paper
lit cites <id> [--hops N]        Get papers that cite this paper
lit path <a> <b> [--max-hops N]  Shortest citation path between two papers
lit download <id> [--source] [--url-only] [--dir DIR]
                                 Download PDF; --source for arXiv LaTeX source
lit read <id>                    Locate paper text; auto-downloads arXiv PDFs
lit add <id> <bib_file>          Fetch BibTeX and append to file
lit misc <key> <bib_file> -t TITLE -y YEAR -a AUTHOR ...
                                 Append hand-rolled @misc entry
lit remove <key> <bib_file>      Remove an entry by citekey
lit verify <bib_file> [-j N]     Verify .bib entries against APIs
lit clean <bib_file> [--apply] [--prune] [--tex DIR ...]
                                 Scan for malformed entries, dupes, orphans
lit check [--fix] [--conflicts]  Check DB<->filesystem consistency
lit db stats|rebuild|rollback    Database operations
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
| `all` | All sources | Merge results |
| *(none)* | Cascade | OA -> SS -> CR -> books |

### Environment

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_CACHE_DIR` | `etc/lit/cache` (relative to binary) | Cache directory |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `NO_COLOR` | *(unset)* | Set to any non-empty value to disable color |
| `LIT_EMAIL` | `lit-cli@users.noreply.github.com` | Email for Unpaywall API |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids shared rate limits) |

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
make test-unit     # cargo test (94 tests)
make test-bats     # bats test/lit.bats (26 tests)
```

## Project structure

```
src/
  main.rs           CLI entry point (clap)
  detect.rs         Input type detection + normalization
  citekey.rs        BibTeX key generation (lastname2017word)
  cache.rs          File-based cache with TTL
  http.rs           HTTP client (reqwest blocking, cache-aware)
  format.rs         Colored output, truncation
  bibtex.rs         BibTeX parsing and generation
  api/
    openalex.rs     OpenAlex API
    semantic_scholar.rs  Semantic Scholar API
    crossref.rs     CrossRef API
    dblp.rs         DBLP API
    arxiv.rs        arXiv API (XML)
    openlibrary.rs  OpenLibrary API
    unpaywall.rs    Unpaywall API
  cmd/
    search.rs       Search with source selection + cascade
    refs.rs         Paper references
    cites.rs        Paper citations
    pdf.rs          Open-access PDF finder
    source.rs       arXiv source download
    open.rs         Open in browser
    add.rs          Fetch + append BibTeX
    verify.rs       Parallel .bib verification
test/
  lit.bats          Integration tests (26 tests)
  cache/            Cached API responses for offline testing
```
