# lit — Clio / EZProxy integration

Columbia-specific integration for institutional PDF access (EZProxy) and local
catalog search (MARCXML bulk index). Both features are optional and degrade
gracefully when not configured.

---

## Overview

| Feature | Command | Requires |
|---|---|---|
| Catalog search | `lit search` (automatic) | `lit clio sync` run once |
| Download via EZProxy | `lit download <doi>` | Active EZProxy session |
| Cookie status | `lit clio auth` | — |
| Build/refresh catalog | `lit clio sync` | Network, ~6 GB disk |

---

## Part 1: EZProxy download

### What EZProxy does

Columbia's EZProxy rewrites publisher URLs so authenticated Columbia users can
access paywalled content. A session cookie at `ezproxy.cul.columbia.edu` is
issued after Columbia CAS login and is valid for ~8 hours.

### Setting up

1. In Chrome, navigate to any Clio "Full Text" link to establish an EZProxy
   session (CAS login happens automatically if you're signed in).
2. From the project root:
   ```bash
   python3 bin/clio-auth.py
   ```
   This exports the EZProxy session cookie to `.cache/lit/clio/cookies.txt`
   in Netscape cookie file format. The file is gitignored.

3. Check status:
   ```bash
   lit clio auth
   # → EZProxy session: 12 cookie(s) found
   # → Expiry: 2026-05-21 – 2026-05-22
   ```

When `.cache/lit/clio/cookies.txt` is present, `lit download <doi>` prints the
EZProxy URL alongside any open-access URL from Unpaywall:

```bash
lit download 10.1080/00031305.2016.1200489
# Title: Statistics and Causal Inference
# PDF: https://arxiv.org/pdf/...          ← open access (if found)
# EZProxy: https://doi-org.ezproxy.cul.columbia.edu/10.1080/...
```

For bulk downloads of a paper set, use `bin/download-cev-pdfs.py` which calls
`curl -b cookies.txt` against the EZProxy URL for each entry in `cev/refs.bib`.

### URL rewriting rule

Publisher domain → EZProxy subdomain:

```
https://onlinelibrary.wiley.com/...
→ https://onlinelibrary-wiley-com.ezproxy.cul.columbia.edu/...
```

Rule: replace dots in the hostname with hyphens, append
`.ezproxy.cul.columbia.edu`. For DOI-based access, route through
`https://doi-org.ezproxy.cul.columbia.edu/<doi>`.

### Cookie file location

`lit download` and `bin/download-cev-pdfs.py` both look for cookies by walking
up from `cwd` until they find `.cache/lit/clio/cookies.txt`. Running from
anywhere inside the project tree works.

### Expiry and refresh

Sessions last ~8 hours. When expired, re-run `python3 bin/clio-auth.py` from
the project root (requires Chrome with an active Clio session).

`bin/clio-auth.py` uses the `browser-cookie3` library and the macOS Keychain
to decrypt Chrome's cookie store. Install once:

```bash
pip install browser-cookie3
```

---

## Part 2: Catalog search

### Data source

Columbia publishes its full catalog as MARCXML under CC0:

```
https://lito.cul.columbia.edu/extracts/ColumbiaLibraryCatalog/full/
```

93 gzipped files (`extract-001.xml.gz` … `extract-093.xml.gz`), each ~64 MB
compressed. Updated monthly. No API key required.

There is no queryable live API — the Ex Libris Primo REST API requires an
institutional key not exposed publicly. The bulk export is the supported
access path.

### First-time setup

```bash
lit clio sync
```

Downloads all 93 files, parses MARC records, and builds a local SQLite FTS5
index. Downloads run 3 at a time with exponential-backoff retries.
Expect ~30–60 minutes depending on network speed.

The index is stored at `etc/lit/clio.db` (sibling to `lit.db`).

### DB path

`clio.db` is found by the same resolution as `lit.db`:

- `LIT_CLIO_DB_PATH` env var, if set
- Otherwise: `<exe_parent>/../etc/lit/clio.db`

When the binary is installed at `/usr/local/bin/lit`, this resolves to
`/usr/local/etc/lit/clio.db`. Symlink it to the project to keep it in the
right place:

```bash
ln -sf /Users/kaizhan/ice/etc/lit/clio.db /usr/local/etc/lit/clio.db
```

(The project already does this for `lit.db`.)

### Resuming and re-syncing

Each file's completion is recorded in `clio_meta` (`key = 'file:extract-NNN.xml.gz'`).
If the sync is interrupted, re-running resumes from where it left off.

A completed sync is guarded by a 30-day check:

```bash
lit clio sync           # skips if synced within 30 days
lit clio sync --force   # clears index and re-syncs unconditionally
lit clio sync --check   # report: record count, files done, last sync date
```

### MARC field mapping

| lit field | MARC tag | Subfield / note |
|---|---|---|
| `title` | 245 | `$a $b` joined |
| `authors` | 100, 700 | `$a`, joined with `, ` |
| `year` | 008 bytes 7–10 | fallback: 260/264 `$c` (first 4-digit run) |
| `isbn` | 020 | `$a` (first) |
| `issn` | 022 | `$a` (first) |
| `doi` | 856 | `$u` containing `doi.org` |
| `url` | 856 ind2=0 | `$u` (first full-text link) |
| `online` | 856 ind2=0 | true when any full-text 856 present |
| `publisher` | 260 or 264 | `$b` |

### Using the catalog in search

After `lit clio sync`, the Clio index is tried automatically before remote APIs:

```bash
lit search "lord paradox"           # local lit DB → Clio → SS → OA (cascade)
lit search --source clio "ancova"   # Clio only (instant, offline)
lit search --source oa "attention"  # OpenAlex only (skips Clio)
```

Clio results that have `online=true` (a full-text 856 link) are flagged as
directly downloadable via EZProxy.

---

## What this is not

- **Not a general EZProxy client.** Columbia-specific.
  `ezproxy.cul.columbia.edu` is hardcoded.

- **Not a security boundary.** The cookie file is a short-lived plaintext
  session token. Don't commit it; don't share it.

- **Not a replacement for OpenAlex on recent preprints.** Clio indexes
  the published catalog; arXiv preprints appear in OpenAlex first.
  Use `--source oa` or `--source all` for recent CS/ML papers.

---

## Files

| Path | Purpose |
|---|---|
| `bin/clio-auth.py` | Export Chrome EZProxy cookies to `.cache/lit/clio/cookies.txt` |
| `bin/download-cev-pdfs.py` | Bulk-download cev papers via EZProxy |
| `.cache/lit/clio/cookies.txt` | EZProxy session cookies (gitignored, ~8h TTL) |
| `etc/lit/clio.db` | Local FTS5 catalog index (~2 GB after full sync) |
| `src/api/clio.rs` | MARCXML parser, FTS5 schema, search function |
| `src/cmd/clio.rs` | `lit clio auth` and `lit clio sync` commands |
| `docs/CLIO.md` | This document |
