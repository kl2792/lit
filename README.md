# lit

Command-line tool for searching, fetching, and managing academic papers.

## Install

```
cargo install --path .
```

Requires Rust 1.85+. Builds two binaries: `lit` (CLI) and `lit-mcp` (MCP server).

## Plugin Installation (Claude Code)

If you use [Claude Code](https://claude.ai/code):

1. Install the binary: `cargo install --path .`
2. Add the plugin: `/plugin marketplace add kl2792/lit`
3. Install: `/plugin install lit@kl2792-lit`

## Quick start

```
lit 2006.11239                        # arXiv lookup
lit 10.1145/3442188.3445899           # DOI lookup
lit https://arxiv.org/abs/2006.11239  # arXiv URL
lit 978-0262039246                    # ISBN lookup
lit "attention is all you need"       # search
```

Paste any identifier or URL and lit auto-detects the type, fetches metadata, and prints a summary with BibTeX.

## CLI commands

| Command | Description |
|---------|-------------|
| `lit <input>` | Auto-detect input type and look up paper |
| `lit search <query>` | Search local DB (add `--remote` or `-s` for API search) |
| `lit refs <id>` | Get references of a paper (supports `--hops`, `--max-papers`) |
| `lit cites <id>` | Get citing papers (supports `--hops`, `--max-papers`) |
| `lit path <id_a> <id_b>` | Find shortest citation path between two papers |
| `lit download <id>` | Download PDF (add `--source` for arXiv LaTeX, `--url-only` to print URL) |
| `lit add <id> <bib_file>` | Fetch BibTeX and append to .bib file |
| `lit verify <bib_file>` | Verify .bib entries against APIs (`-j N` for parallelism) |
| `lit clean <bib_file>` | Scan for malformed entries, duplicates, orphans (`--apply` to fix) |
| `lit check` | Check DB/filesystem consistency (`--fix` to repair, `--conflicts` to report) |
| `lit db stats` | Show database statistics |
| `lit db rebuild` | Rebuild database from source.yaml files |

### Global flags

```
-v, --verbose        Full details
-b, --bib[=FILE]     Output BibTeX (append to FILE if given, stdout if not)
-o, --open           Open paper in browser
    --json           Machine-readable JSON output
    --no-cache       Bypass cache, fetch fresh
    --no-color       Disable colored output
```

### Search sources (`-s`)

| Flag | Source |
|------|--------|
| `oa` | OpenAlex (default) |
| `ss` | Semantic Scholar |
| `cr` | CrossRef |
| `dblp` | DBLP |
| `book` | OpenLibrary |
| `all` | All sources merged |

## MCP server

`lit-mcp` is a Model Context Protocol server that exposes lit's functionality to LLM agents. It reads JSON-RPC 2.0 from stdin and writes responses to stdout.

### Configuration

Add to `.mcp.json`:

```json
{
  "mcpServers": {
    "lit": {
      "command": "lit-mcp"
    }
  }
}
```

### Tools

| Tool | Description |
|------|-------------|
| `search` | Search papers (local DB or remote APIs) |
| `lookup` | Look up a paper by arXiv ID, DOI, or ISBN |
| `read` | Get path to a paper's full text (auto-downloads from arXiv if needed) |
| `add` | Fetch BibTeX and append to .bib file |
| `misc` | Add a @misc BibTeX entry (blog post, unpublished work) |
| `refs` | Get references of a paper |
| `cites` | Get citing papers |
| `path` | Find shortest citation path between two papers |
| `clean` | Scan .bib file for problems |

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_DB_PATH` | `~/Library/Application Support/lit/lit.db` (macOS) or `~/.local/share/lit/lit.db` (Linux) | SQLite database path |
| `LIT_PDF_DIR` | `~/Library/Application Support/lit/pdf/` (macOS) or `~/.local/share/lit/pdf/` (Linux) | Paper storage directory |
| `LIT_PDF_EXTRACTOR` | `pdftotext` | Absolute path to PDF-to-text command (called as `<cmd> <pdf> <output>`) |
| `LIT_EMAIL` | `lit-user@example.com` | Email for Unpaywall API (set this for production use) |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids shared rate limits) |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `LIT_TTL_SEARCH` | `86400` (24h) | Cache TTL for search results (seconds, 0 = no cache) |
| `LIT_TTL_LOOKUP` | `604800` (7d) | Cache TTL for DOI/arXiv/ISBN lookups (seconds, 0 = no cache) |
| `NO_COLOR` | *(unset)* | Disable colored output |

### Configuration file

Settings can also be specified in `.litconfig` files (TOML format). Files are read in order of increasing priority:

1. `/etc/litconfig` — system-wide defaults
2. `~/.litconfig` — user global
3. `<cwd>/.litconfig` — project-specific
4. Environment variables — override everything

Later sources override earlier ones. Missing or invalid files are silently skipped.

Example `~/.litconfig`:

```toml
[core]
db_path = "~/.local/share/lit/lit.db"
pdf_dir = "~/papers"
email = "me@example.com"
timeout = 15

[cache]
ttl_search = 86400
ttl_lookup = 604800

[api]
s2_key = "your-semantic-scholar-key"

[extract]
pdf_extractor = "/usr/local/bin/pdf-extract.sh"
```

Path values support tilde expansion (`~` expands to `$HOME`).

## Paper storage

Papers are stored in directories named by citekey under `LIT_PDF_DIR`:

```
pdf/
  conmy2023acdc/
    paper.pdf         # downloaded PDF
    paper.txt         # extracted plain text
    source.yaml       # metadata (arXiv ID, DOI, title, etc.)
```

- Each directory contains `paper.pdf`, `paper.txt` (extracted text), and `source.yaml` (metadata).
- The `read` command auto-extracts text on first access (using `pdftotext` or the command set by `LIT_PDF_EXTRACTOR`).
- The `download --source` command fetches arXiv LaTeX source into the same directory.

## Project structure

```
src/
  main.rs              CLI entry point (clap)
  lib.rs               Library root, re-exports
  detect.rs            Input type detection + normalization
  citekey.rs           BibTeX cite key generation
  bibtex.rs            BibTeX parsing and generation
  db.rs                SQLite database (papers, citations, cache)
  http.rs              HTTP client (reqwest, cache-aware)
  format.rs            Colored output, truncation
  api/
    openalex.rs        OpenAlex API
    semantic_scholar.rs Semantic Scholar API
    crossref.rs        CrossRef API
    dblp.rs            DBLP API
    arxiv.rs           arXiv API (XML)
    openlibrary.rs     OpenLibrary API
    unpaywall.rs       Unpaywall API
  cmd/
    search.rs          Search (local FTS + remote cascade)
    refs.rs            Paper references (BFS)
    cites.rs           Paper citations (BFS)
    path.rs            Citation path finding
    download.rs        PDF and LaTeX source download
    add.rs             Fetch + append BibTeX
    verify.rs          Parallel .bib verification
    clean.rs           .bib linting and deduplication
    check.rs           DB consistency checks + rebuild
    read.rs            Full-text path resolution
    open.rs            Open in browser
    misc.rs            Manual @misc entry creation
  bin/
    lit-mcp.rs         MCP server (JSON-RPC 2.0 over stdio)
test/
  lit.bats             Integration tests (bats)
  cache/               Cached API responses for offline testing
```

## License

MIT
