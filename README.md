# lit

Command-line tool for searching, fetching, and managing academic papers.

## Install

### Claude Code plugin (recommended)

```
/plugin marketplace add kl2792/lit
/plugin install lit@kl2792-lit
```

### From source (requires Rust 1.85+)

```bash
cargo install --git https://github.com/kl2792/lit
```

Builds two binaries: `lit` (CLI) and `lit-mcp` (MCP server).

### Development

```bash
git clone https://github.com/kl2792/lit && cd lit
cargo install --path .
```

## Quick start

```bash
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
| `lit search <query>` | Search (local DB first, auto-falls back to remote APIs if no results) |
| `lit search <query> --remote` | Search remote APIs directly (use `-s oa/ss/cr/dblp/book/all`) |
| `lit refs <id>` | Get references of a paper (`--hops N`, `--max-papers N`) |
| `lit cites <id>` | Get citing papers (`--hops N`, `--max-papers N`) |
| `lit path <id_a> <id_b>` | Find shortest citation path between two papers |
| `lit download <id>` | Download PDF (`--source` for arXiv LaTeX, `--url-only` for URL) |
| `lit read <query>` | Get path to paper's full text (`-p` to print contents) |
| `lit add <id> <bib_file>` | Fetch BibTeX and append to .bib file |
| `lit verify <bib_file>` | Verify .bib entries against APIs (`-j N` for parallelism) |
| `lit clean <bib_file>` | Scan for malformed entries, duplicates, orphans (`--apply` to fix) |
| `lit check` | Check DB/filesystem consistency (`--fix` to repair) |
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
    --version        Show version
```

## MCP server

`lit-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io/) server that exposes lit's functionality to LLM agents. It reads JSON-RPC 2.0 from stdin and writes responses to stdout.

### Setup

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
| `refs` | Get references of a paper (paginated) |
| `cites` | Get citing papers (paginated) |
| `path` | Find shortest citation path between two papers |
| `clean` | Scan .bib file for problems |

## Configuration

All settings can be specified via environment variables, `.litconfig` files, or both. Env vars take highest priority.

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_DB_PATH` | Platform-specific (see below) | SQLite database path |
| `LIT_PDF_DIR` | Platform-specific (see below) | Paper storage directory |
| `LIT_PDF_EXTRACTOR` | `pdftotext` | Absolute path to PDF-to-text command (`<cmd> <pdf> <output>`) |
| `LIT_EMAIL` | `lit-user@example.com` | Email for Unpaywall API |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids rate limits) |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `LIT_TTL_SEARCH` | `86400` (24h) | Cache TTL for search results (seconds, 0 = no cache) |
| `LIT_TTL_LOOKUP` | `604800` (7d) | Cache TTL for lookups (seconds, 0 = no cache) |
| `NO_COLOR` | *(unset)* | Disable colored output |

### Platform defaults

| Platform | Database | Paper storage |
|----------|----------|---------------|
| macOS | `~/Library/Application Support/lit/lit.db` | `~/Library/Application Support/lit/pdf/` |
| Linux | `~/.local/share/lit/lit.db` | `~/.local/share/lit/pdf/` |
| Windows | `%APPDATA%\lit\lit.db` | `%APPDATA%\lit\pdf\` |

### Configuration file

Settings can also be specified in `.litconfig` files (TOML format), read in order of increasing priority:

1. `/etc/litconfig` — system-wide defaults
2. `~/.litconfig` — user global
3. `<cwd>/.litconfig` — project-specific
4. Environment variables — override everything

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
pdf_extractor = "/usr/local/bin/my-pdf-extractor"
```

Path values support tilde expansion (`~` expands to `$HOME`).

## Paper storage

Papers are stored in directories named by citekey:

```
<LIT_PDF_DIR>/
  vaswani2017attention/
    paper.pdf         # downloaded PDF
    paper.txt         # extracted text (auto-generated on first read)
    source.yaml       # metadata (arXiv ID, DOI, title)
```

The `read` command auto-extracts text using `pdftotext` or the command set by `LIT_PDF_EXTRACTOR`. Use `download --source` to fetch arXiv LaTeX source.

## License

MIT
