# lit — Clio / EZProxy integration

This document covers the design for institutional PDF access via Columbia's
EZProxy, authenticated through Clio (Columbia's library catalog).

---

## Motivation

`lit download` currently uses Unpaywall to find open-access PDFs.
For paywalled articles, it returns nothing.
Columbia students have institutional access to most of these articles through
EZProxy, which rewrites publisher URLs to proxy authenticated requests.

Adding EZProxy as a download backend lets `lit download` succeed on paywalled
articles without changing the calling interface.

---

## Auth model

EZProxy issues a session cookie (`ezproxy`, or similar) after a Columbia CAS
login. This cookie is valid for a single browser session (~8 hours).

**Storage:** `.cache/lit/clio/cookies.txt` (Netscape cookie file format).
**Scope:** only `ezproxy.cul.columbia.edu` cookies — not the full browser jar.
**Populated by:** `python3 bin/clio-auth.py` (must run from a real terminal;
requires macOS Keychain access to decrypt Chrome's cookie store).

The cache file is gitignored. It is never transmitted anywhere — it is only
read by `curl` invocations on the local machine.

---

## URL rewriting

EZProxy authenticates by rewriting publisher domains:

```
https://onlinelibrary.wiley.com/doi/pdfdirect/10.1002/jae.788
→ https://onlinelibrary-wiley-com.ezproxy.cul.columbia.edu/doi/pdfdirect/10.1002/jae.788
```

Rule: replace dots in the hostname with hyphens, append `.ezproxy.cul.columbia.edu`.

For DOI-based access, routing through `doi-org.ezproxy.cul.columbia.edu/<doi>`
lets EZProxy follow the DOI redirect and serve the landing page authenticated,
from which `lit download` can extract the PDF link.

---

## `lit download` priority order

When `.cache/lit/clio/cookies.txt` exists:

1. **EZProxy direct** — rewrite the paper's URL through EZProxy, curl with
   the session cookie, check for `%PDF` magic bytes.
2. **EZProxy via DOI** — if the paper has a DOI, try
   `https://doi-org.ezproxy.cul.columbia.edu/<doi>`.
3. **Unpaywall** — original fallback for genuinely open-access papers.

When the cache file is absent, skip steps 1–2 and go straight to Unpaywall
(existing behavior, no regression for users without institutional access).

---

## New command: `lit clio-auth`

A thin wrapper around `bin/clio-auth.py` that belongs in the CLI:

```
lit clio-auth            # check cache status + expiry
lit clio-auth --refresh  # print instructions to refresh (can't do it headlessly)
```

`lit clio-auth` without flags reports:
- Whether `.cache/lit/clio/cookies.txt` exists
- Cookie count and estimated expiry (from the `expires` field)
- Whether cookies are still valid (basic check: at least one non-expired entry)

It cannot refresh the session itself — that requires a real browser and Keychain
access. It just tells the user when to re-run `bin/clio-auth.py`.

---

## Clio as the default search backend

### Why not OpenAlex by default

OpenAlex is well-suited for recent CS/ML papers with arXiv IDs.
For the kind of work `lit` is actually used for — older statistics and social-science
journals, pre-1990 methodology papers, books and book chapters — coverage is poor.
Clio (Columbia's Ex Libris Primo catalog) indexes all of these and knows which ones
Columbia has full-text access to, making it a better default for the actual search load.

The EZProxy download integration makes the pairing complete:
a Clio search result carries availability metadata, so `lit download` can immediately
attempt an authenticated fetch rather than falling back to Unpaywall and failing.

### Primo REST API

Endpoint: `https://columbia.primo.exlibrisgroup.com/primaws/rest/pub/pnxs`

Key parameters:

| Parameter | Value | Notes |
|---|---|---|
| `q` | `any,contains,<query>` | Full-text keyword. Also `title,contains,...`, `creator,contains,...` |
| `vid` | `01COLU_INST:COLU` | Columbia's Primo view |
| `tab` | `Everything` | All catalog scopes |
| `search_scope` | `MyInst_and_CI` | Institution + CI partners |
| `offset` | 0 | Pagination |
| `limit` | 10–50 | Max results per page |
| `lang` | `en` | Response language |

No API key is required for read-only catalog searches.
Rate limit: polite usage; no published threshold from ExLibris.

### Result shape mapping

Primo returns `data[].pnx` records. Mapping to lit's standard schema:

| lit field | Primo path |
|---|---|
| `title` | `pnx.display.title[0]` |
| `authors` | `pnx.display.creator` (split on `;`) |
| `year` | `pnx.display.creationdate[0]` |
| `doi` | `pnx.addata.doi[0]` (if present) |
| `isbn` | `pnx.addata.isbn[0]` (books) |
| `abstract` | `pnx.display.description[0]` |
| `venue` | `pnx.display.publisher[0]` or `pnx.display.ispartof[0]` |
| `available` | `pnx.delivery.availability[0] == "fulltext_linktorsrc"` |
| `ezproxy_url` | `pnx.links.linktorsrc[0]` prefixed through EZProxy rewriter |

`available` is the key field: when true, the record has institutional full-text
access and `lit download` should route through EZProxy first.

### `lit search` source priority

```
lit search "query"
```

Default source order (no `--source` flag):

1. **Local DB** — instant, full-text over previously-fetched metadata.
2. **Clio** — Columbia's Primo catalog. Broad coverage, includes books and
   older journals, returns availability metadata.
3. **OpenAlex** — fallback for preprints and recent papers not yet in Clio.

`--source` flag values (unchanged interface, new defaults):

| Flag | Behavior |
|---|---|
| `--source clio` | Clio only |
| `--source oa` | OpenAlex only (previous default) |
| `--source ss` | Semantic Scholar |
| `--source all` | Merge across all remote sources |

When `--remote` is omitted, search is local DB only (unchanged behavior — the
source order above applies only when `--remote` is passed).

### Search → download integration

When a Clio result has `available: true`, `lit add` and `lit download` can skip
Unpaywall and go straight to EZProxy:

```bash
lit search --remote "lord's paradox" --source clio --json
# → result includes "available": true, "ezproxy_url": "https://..."

lit add <doi> references.bib    # internally: EZProxy first, Unpaywall fallback
```

Results without `available: true` fall through to the standard download priority
chain (EZProxy via URL rewriting → Unpaywall).

---

## What this is not

- **Not a general EZProxy client.** This is Columbia-specific.
  The institution is hardcoded (`ezproxy.cul.columbia.edu`).
  Generalising to arbitrary EZProxy hosts is a separate concern.

- **Not a security boundary.** The cookie file is plaintext on disk.
  It contains a short-lived session token, not a password.
  Treat it like a browser session: don't commit it, don't share it.

- **Not a replacement for OpenAlex on recent preprints.** Clio indexes
  published catalog entries; arXiv preprints land in OpenAlex first.
  `--source all` or `--source oa` remain the right flags for that case.

---

## Implementation plan

1. `bin/clio-auth.py` — already implemented. Exports EZProxy cookies from
   Chrome to `.cache/lit/clio/cookies.txt`.

2. `bin/download-cev-pdfs.py` — already updated. Uses cookie file + EZProxy
   URL rewriting to curl paywalled PDFs for all `cev/refs.bib` entries.

3. `lit download` integration — pending. Add EZProxy fallback to the Rust
   download command: check for cookie file, rewrite URL, curl, fall back to
   Unpaywall. No new flags; behavior is automatic when cookie file is present.

4. `lit clio-auth` subcommand — pending. Status check and refresh instructions.

5. `lit search --source clio` — pending. Implement Primo REST client in Rust.
   Map `pnx` records to the standard lit schema. Make Clio the first remote
   source tried when `--remote` is passed without `--source`.

6. `lit search` default source — pending. After (5), change the default remote
   source from OpenAlex to Clio, with OpenAlex as the automatic fallback.
