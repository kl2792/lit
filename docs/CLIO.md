# lit — Clio / EZProxy integration

This document covers the design for institutional PDF access and catalog search
via Columbia's Clio library catalog (EZProxy + MARCXML bulk data).

---

## Motivation

`lit download` currently uses Unpaywall to find open-access PDFs.
For paywalled articles, it returns nothing.
Columbia students have institutional access to most of these articles through
EZProxy, which rewrites publisher URLs to proxy authenticated requests.

`lit search --remote` currently defaults to OpenAlex, which has poor coverage
of older statistics and social-science journals, books, and book chapters —
the material most commonly needed in this workspace.

This integration adds two things:
- EZProxy as a download backend (paywalled PDFs via auth cookies).
- A local Clio catalog index for search (full Columbia holdings, offline-capable).

---

## Part 1: EZProxy download

### Auth model

EZProxy issues a session cookie after a Columbia CAS login.
This cookie is valid for a single browser session (~8 hours).

**Storage:** `.cache/lit/clio/cookies.txt` (Netscape cookie file format).
**Scope:** only `ezproxy.cul.columbia.edu` cookies — not the full browser jar.
**Populated by:** `python3 bin/clio-auth.py` (requires macOS Keychain access to
decrypt Chrome's cookie store; must run once from a real terminal, or from Claude
Code when Keychain access is approved).

The cache file is gitignored. It is never transmitted anywhere — it is only
read by `curl` invocations on the local machine.

### URL rewriting

EZProxy authenticates by rewriting publisher domains:

```
https://onlinelibrary.wiley.com/doi/pdfdirect/10.1002/jae.788
→ https://onlinelibrary-wiley-com.ezproxy.cul.columbia.edu/doi/pdfdirect/10.1002/jae.788
```

Rule: replace dots in the hostname with hyphens, append `.ezproxy.cul.columbia.edu`.

For DOI-based access, routing through `doi-org.ezproxy.cul.columbia.edu/<doi>`
lets EZProxy follow the DOI redirect and serve the landing page authenticated,
from which `lit download` can extract the PDF link.

### `lit download` priority order

When `.cache/lit/clio/cookies.txt` exists:

1. **EZProxy direct** — rewrite the paper's URL through EZProxy, curl with
   the session cookie, check for `%PDF` magic bytes.
2. **EZProxy via DOI** — if the paper has a DOI, try
   `https://doi-org.ezproxy.cul.columbia.edu/<doi>`.
3. **Unpaywall** — original fallback for genuinely open-access papers.

When the cache file is absent, skip steps 1–2 and go straight to Unpaywall
(existing behavior, no regression for users without institutional access).

### New command: `lit clio-auth`

```
lit clio-auth            # check cache status + expiry
lit clio-auth --refresh  # print instructions to refresh
```

Reports whether `.cache/lit/clio/cookies.txt` exists, cookie count, and
estimated expiry. Cannot refresh headlessly — just tells you when to re-run
`bin/clio-auth.py`.

---

## Part 2: Clio catalog search

### Why not OpenAlex by default

OpenAlex is well-suited for recent CS/ML papers with arXiv IDs.
For older statistics and social-science journals, pre-1990 methodology papers,
books, and book chapters, coverage is poor. Clio indexes all of these.
Pairing Clio search with EZProxy download closes the loop: search results
point directly to downloadable content.

### Data source: MARCXML bulk export

Columbia publishes its full catalog as MARCXML under CC0:

```
https://lito.cul.columbia.edu/extracts/ColumbiaLibraryCatalog/full/
```

Files: `extract-001.xml.gz` … `extract-093.xml.gz`, each ~64 MB compressed,
updated monthly. No API key required.

There is no queryable live API — the Primo REST API requires an institutional
key that Columbia does not expose publicly. The bulk export is the supported
access path.

### New command: `lit clio-sync`

Downloads and indexes the MARCXML into a local SQLite FTS5 table:

```bash
lit clio-sync          # full sync (first run, or monthly refresh)
lit clio-sync --check  # report index age and record count, no download
```

The sync is explicit, not automatic. It downloads ~6 GB and takes several
minutes. Run at most monthly (Columbia updates the catalog monthly).

Index stored at `.cache/lit/clio/catalog.db`.

### MARC field mapping

Key fields extracted during sync:

| lit field | MARC tag | Subfield |
|---|---|---|
| `title` | 245 | `$a $b` |
| `authors` | 100, 700 | `$a` |
| `year` | 008 bytes 7–10 or 260/264 | `$c` |
| `isbn` | 020 | `$a` |
| `issn` | 022 | `$a` |
| `doi` | 856 | `$u` matching `doi.org` |
| `url` | 856 | `$u` (fulltext links) |
| `location` | 852 | `$a` (physical vs. online) |
| `publisher` | 260 or 264 | `$b` |

Records with `856 $u` containing an EZProxy or publisher URL are indexed as
`online: true` — these are directly downloadable via EZProxy when cookies exist.

### `lit search` with Clio index

After `lit clio-sync`, the local index becomes a search source:

```bash
lit search "lord paradox"                    # local lit DB only (unchanged)
lit search --remote "lord paradox"           # Clio index first, then OpenAlex
lit search --source clio "lord paradox"      # Clio index only
lit search --source oa "lord paradox"        # OpenAlex only (previous default)
```

`--remote` without `--source` now defaults to Clio, with OpenAlex as fallback
for records not found (preprints, papers published after the last sync).

Because the Clio index is local, `--source clio` queries are instant regardless
of network state.

### Search → download integration

Records with `online: true` feed directly into the EZProxy download path:

```bash
lit search --remote "lord paradox" --json
# → result includes "online": true, "url": "https://..."

lit add <doi> references.bib    # EZProxy first if Clio says it's online
```

Records without `online: true` fall through to the standard chain
(EZProxy URL rewriting → Unpaywall).

---

## What this is not

- **Not a general EZProxy client.** Columbia-specific.
  The institution is hardcoded (`ezproxy.cul.columbia.edu`).

- **Not a security boundary.** The cookie file is plaintext on disk.
  It contains a short-lived session token, not a password.
  Don't commit it, don't share it.

- **Not a replacement for OpenAlex on recent preprints.** Clio indexes
  published catalog entries; arXiv preprints land in OpenAlex first.
  Use `--source all` or `--source oa` for that case.

---

## Implementation plan

1. `bin/clio-auth.py` — already implemented. Exports EZProxy cookies from
   Chrome to `.cache/lit/clio/cookies.txt`.

2. `bin/download-cev-pdfs.py` — already implemented. Uses cookie file +
   EZProxy URL rewriting to curl paywalled PDFs for all `cev/refs.bib` entries.

3. `lit download` integration — pending. Add EZProxy fallback to the Rust
   download command: check for cookie file, rewrite URL, curl, fall back to
   Unpaywall. No new flags; behavior is automatic when the cookie file is present.

4. `lit clio-auth` subcommand — pending. Status check and refresh instructions.

5. `lit clio-sync` — pending. Python script (or Rust command) that streams
   MARCXML files, extracts key fields, and populates `.cache/lit/clio/catalog.db`
   with an FTS5 table over title, authors, year, isbn, issn, doi, url, online.

6. `lit search --source clio` — pending. Query the local FTS5 table, map rows
   to lit's standard schema, make Clio the first source tried when `--remote` is
   passed without `--source`.
