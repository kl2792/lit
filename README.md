# lit

Your research library, from the terminal.

`lit` is a command-line tool for searching, citing, and reading academic papers. It searches 6 APIs (Semantic Scholar, OpenAlex, CrossRef, DBLP, arXiv, OpenLibrary), caches results locally, and works as a [Claude Code](https://claude.ai/code) plugin — your AI agent can search, read, and cite papers autonomously.

```bash
# Search → add to bibliography → download → read
$ lit "attention is all you need"
1. Vaswani 2017 | Attention is All you Need | arXiv:1706.03762
2. Subakan 2020 | Attention Is All You Need In Speech Separation | arXiv:2010.13154
3. Choi 2020 | Channel Attention Is All You Need for Video Frame... | DOI:10.1609/AAAI.V34I07.6693
...

$ lit add 1706.03762 refs.bib
Added vaswani2017attention to refs.bib

$ lit download 1706.03762
Saved to ~/.local/share/lit/pdf/vaswani2017attention/paper.pdf

$ lit read 1706.03762 -p | head -20
Attention Is All You Need
Ashish Vaswani, Noam Shazeer, Niki Parmar...
```

Explore the citation graph:

```bash
$ lit cites 1706.03762                   # 120,000+ papers that cited this
$ lit refs 1706.03762                    # what Vaswani et al. cited
$ lit path 1706.03762 2002.04745         # shortest citation path between two papers
```

Keep your .bib clean:

```bash
$ lit verify refs.bib                    # check titles, years, DOIs against APIs
$ lit clean refs.bib --apply             # remove duplicates + malformed entries
```

## Install

### Claude Code plugin (recommended, no Rust needed)

```
/plugin marketplace add kl2792/lit
/plugin install lit@kl2792-lit
```

The plugin bundles pre-built binaries. Use it from any Claude Code conversation — Claude can search papers, add citations, and read full text.

### From source

Requires [Rust](https://rustup.rs/) 1.85+.

```bash
cargo install --git https://github.com/kl2792/lit
```

Builds two binaries: `lit` (CLI) and `lit-mcp` (MCP server).

### Dependencies

Text extraction (`lit read`) requires `pdftotext`:

```bash
brew install poppler         # macOS
sudo apt install poppler-utils  # Linux
```

All other commands (search, add, download, refs, cites, verify, clean) work without it. You can also set `LIT_PDF_EXTRACTOR` to a custom extractor (see [Configuration](#configuration)).

## CLI commands

| Command | Description |
|---------|-------------|
| `lit <input>` | Auto-detect input type and look up paper |
| `lit search <query>` | Search (local DB first, auto-falls back to remote if no results) |
| `lit search <query> --remote` | Search remote APIs directly (`-s oa/ss/cr/dblp/book/all`) |
| `lit refs <id>` | Get references of a paper (`--hops N`, `--max-papers N`) |
| `lit cites <id>` | Get citing papers (`--hops N`, `--max-papers N`) |
| `lit path <id_a> <id_b>` | Find shortest citation path between two papers (BFS over citation graph) |
| `lit download <id>` | Download PDF (`--source` for arXiv LaTeX, `--url-only` for URL) |
| `lit read <query>` | Extract text, return file path (`-p` to print to stdout) |
| `lit add <id> <bib_file>` | Fetch BibTeX and append to .bib file |
| `lit verify <bib_file>` | Check titles, years, and DOIs against 5 APIs (`-j N` for parallelism) |
| `lit clean <bib_file>` | Find malformed entries, duplicates, orphans (`--apply` to fix in place) |
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

`lit-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io/) server for LLM agents. If you installed via the Claude Code plugin, it's already configured. For manual setup, add to `.mcp.json`:

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
| `read` | Get path to full text (auto-downloads arXiv papers) |
| `add` | Fetch BibTeX and append to .bib file |
| `misc` | Add a @misc BibTeX entry (blog post, unpublished work) |
| `refs` | Get references of a paper (paginated) |
| `cites` | Get citing papers (paginated) |
| `path` | Find shortest citation path between two papers |
| `clean` | Scan .bib file for problems |

### Example: Claude Code conversation

```
You: Find papers on causal inference in RL, add the top 3 to refs.bib,
     then show me what Buesing 2019 cites.

Claude: [searches → finds 10 papers → adds 3 to refs.bib → fetches Buesing's references]

  Added buesing2019woulda to refs.bib
  Added forney2017counterfactual to refs.bib  
  Added lu2018deconfounding to refs.bib

  Buesing et al. (2019) cites 47 papers, including:
  1. Pearl 2009 | Causality
  2. Schulman 2015 | High-Dimensional Continuous Control Using GAE
  3. Mnih 2016 | Asynchronous Methods for Deep RL
  ...
```

Claude handles tool calls automatically — search, add, read, and citation traversal all work without manual steps.

## Configuration

All settings: environment variables, `.litconfig` files, or both. Env vars take highest priority.

| Variable | Default | Description |
|----------|---------|-------------|
| `LIT_DB_PATH` | Platform data dir (see below) | SQLite database path |
| `LIT_PDF_DIR` | Platform data dir (see below) | Paper storage directory |
| `LIT_PDF_EXTRACTOR` | `pdftotext` | PDF-to-text command (`<cmd> <pdf> <output>`) |
| `LIT_EMAIL` | `lit-user@example.com` | Email for Unpaywall API (set to your email for reliable access) |
| `S2_API_KEY` | *(unset)* | Semantic Scholar API key (free, avoids rate limits) |
| `CURL_TIMEOUT` | `15` | HTTP timeout in seconds |
| `LIT_TTL_SEARCH` | `86400` (24h) | Cache TTL for search results (0 = no cache) |
| `LIT_TTL_LOOKUP` | `604800` (7d) | Cache TTL for lookups (0 = no cache) |
| `NO_COLOR` | *(unset)* | Disable colored output |

Data is stored in the platform data directory: `~/Library/Application Support/lit/` (macOS), `~/.local/share/lit/` (Linux), `%APPDATA%\lit\` (Windows).

### Configuration file

Settings can also go in `.litconfig` files (TOML), read in order:

1. `/etc/litconfig` — system-wide
2. `~/.litconfig` — user global
3. `.litconfig` — project-specific (in cwd)
4. Environment variables — override all

```toml
# ~/.litconfig
[core]
pdf_dir = "~/papers"
email = "me@example.com"

[api]
s2_key = "your-key"
```

Path values support `~` expansion.

## Paper storage

Papers are stored by citekey under `LIT_PDF_DIR`:

```
vaswani2017attention/
  paper.pdf         # downloaded PDF
  paper.txt         # extracted text (auto-generated on first read)
  source.yaml       # metadata (arXiv ID, DOI, title)
```

## License

MIT
