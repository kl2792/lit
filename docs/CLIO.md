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

## What this is not

- **Not a full Clio API integration.** Clio (Ex Libris Primo) has a search API,
  but it is redundant with OpenAlex and CrossRef which `lit` already uses.
  The only thing Clio provides that nothing else does is *authenticated PDF access*.
  That is the only thing this integration touches.

- **Not a general EZProxy client.** This is Columbia-specific.
  The institution is hardcoded (`ezproxy.cul.columbia.edu`).
  Generalising to arbitrary EZProxy hosts is a separate concern.

- **Not a security boundary.** The cookie file is plaintext on disk.
  It contains a short-lived session token, not a password.
  Treat it like a browser session: don't commit it, don't share it.

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
