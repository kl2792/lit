# lit — Design

This document explains *why* `lit` is factored the way it is.
For *what* it does and *how* to invoke it, see [`README.md`](../README.md).

---

## Principles

1. **CLI is the boundary.** All bibliographic operations go through `lit`.
   No direct `.bib` editing; no `WebFetch` for papers; no parallel access paths.
   One cache, one rate limiter, one source of truth for citekey formatting.

2. **Identity vs. discovery are separate.**
   Looking up a known identifier and searching for unknown work are different operations
   with different return shapes, different latencies, and different cache behavior.
   They get separate commands.

3. **State-mutating operations are explicit.**
   `add` and `misc` write to disk and return a canonical `entry_key`.
   They are not shell-pipe constructions over `lookup --bib`,
   because they need collision detection, canonicalization, and dedup.

4. **Slow operations are visible.**
   Network-bound commands are flagged in docs and return predictable JSON
   so callers can choose between blocking and backgrounding without surprise.

5. **No source is complete.**
   The `--source` flag exposes the trade-off rather than hiding it behind a single backend.

---

## Command rationale

### Lookup vs. search

| Command | Returns | Backed by |
|---|---|---|
| `lit <id>` | One record (or error) | Cache, then arXiv/DOI/ISBN-specific API |
| `lit search <query>` | Ranked list | OpenAlex by default; `--remote` for fresh fetch |

Two commands, not one with a `--mode` flag.
Lookup is identity-keyed (deterministic input → single result, cache hit rate near 1.0).
Search is exploratory (query → ranked list, no identity).
Collapsing them would force lookup callers to unwrap single-element lists,
and force search callers to wrap single inputs into lists.
They also have disjoint flag surfaces (`--limit`, `--source`, `--remote` apply only to search).

### Citation graph: `refs`, `cites`, `path`

Three commands instead of one parameterised graph command.

`refs` and `cites` hit different backends with different latency profiles.
References are usually in the paper's own metadata: fast, complete, one API call.
Citations require an inverted index across the entire corpus: slow, incomplete, source-dependent.
A unified `lit graph <id> --direction=in|out` would hide a 10× latency difference behind a flag.

`path` is a primitive because the alternative is hundreds of `refs`/`cites` calls.
A 5-hop BFS for the shortest citation path between two papers is one server-side DB query
or hundreds of client-side API calls. Forcing the client to do BFS would either be unusably
slow or require duplicating the server cache locally.

### Retrieval: `download`, `read`

`download` deposits a file. `read` returns text. Different cost, different output shape.

`download` is not part of `lookup` because PDFs are bandwidth-heavy and most lookups don't
need full text — they need to confirm a citekey, fetch BibTeX, or check the abstract.
Auto-downloading would burn disk and bandwidth on every metadata query.

`read` exists as a separate command (rather than `pdftotext` over the cached file) because:
- It hits cached extracted text; re-extracting from PDF is expensive.
- It has an auto-download fallback for arXiv IDs: if the paper isn't cached, fetch it first.
- The format is normalized (text or markdown depending on source format).

### Bib management: `add`, `misc`, `verify`, `clean`

`add` is not `lookup --bib >> file.bib`. Three things shell redirection can't do:
- Detect citekey collisions against existing entries.
- Canonicalise the key to `lastname<year><word>` format.
- Skip the write if the paper is already present under a different key.

It is a stateful merge, not a fetch.

`misc` is the escape hatch for entries with no resolvable identifier:
textbooks, working papers, technical reports, personal communications, software.
Without it, the alternative is hand-editing the `.bib` file,
which bypasses canonicalisation and violates the "one writer" invariant.
Keeping `misc` separate from `add` makes the contract explicit:
`add` requires a real identifier; `misc` is "trust me, here are the fields."

`verify` is online and slow (re-queries every entry).
`clean` is offline and fast (parses the file for structural problems).
Different cost, different signals, different cadence:
clean on every commit, verify monthly or before submission.

### Maintenance: `check`, `db`

`check` covers a different invariant from `verify`/`clean`:
DB ↔ filesystem consistency.
Orphaned PDFs, missing PDFs for indexed papers, DB rows pointing to deleted files.
None of these are visible from a `.bib` file alone.

`db` is the escape hatch for direct DB inspection and recovery.
Schema migration, partial-write recovery, ad-hoc queries.
Hiding it would force users to poke at SQLite directly when something goes wrong.

---

## Output contracts

All commands accept `--json` for machine-readable output.
Schemas below describe the JSON-mode contract; human-mode formatting is unstable.

### `lit <id>` / `lit search`

```json
{
  "title": "...",
  "authors": ["...", "..."],
  "year": "2020",
  "doi": "10.1145/...",       // present when available
  "arxiv_id": "2006.11239",   // present when available
  "abstract": "...",
  "pdf_url": "https://..."
}
```

Search returns an array of these.

### `lit add` / `lit misc`

```json
{
  "entry_key": "ho2020denoising",
  "bib_file": "/abs/path/to/references.bib"
}
```

`entry_key` is the canonical key as written to the file.
**Callers must use this value for `\cite{}`; never construct the key heuristically.**

### `lit refs` / `lit cites`

```json
{
  "results": [ /* paper records, same shape as lookup */ ],
  "offset": 0,
  "page_size": 20,
  "has_more": true
}
```

### `lit path`

```json
{
  "path": ["paper_a_id", "intermediate_id", "paper_b_id"],
  "hops": 2
}
```

### `lit read`

```json
{
  "path": "/abs/path/to/extracted.txt",
  "format": "text",         // "text" | "markdown"
  "auto_downloaded": true   // present only when the PDF was fetched on this call
}
```

### `lit verify` / `lit clean`

Status lines per entry; exit nonzero on any failure.
See README examples.

---

## Cache and rate-limit behavior

- **Location:** `LIT_CACHE_DIR` (default: `etc/lit/cache` relative to binary).
- **TTL:** 24 hours for search results, 7 days for identifier lookups.
- **Invalidation:** `--no-cache` bypasses on a single call.
  No "clear cache" subcommand by design — delete the directory if you need a full reset.
- **Rate limits:** respected per source. `S2_API_KEY` raises Semantic Scholar's shared-pool limit.
- **`LIT_EMAIL`** is sent to Unpaywall to comply with their terms.

---

## Integration patterns (agents / Claude Code)

### Citekey discipline

The canonical citekey is returned in `entry_key` from `lit add --json` and `lit misc --json`.
Use that value verbatim for `\cite{}`. Never derive a key heuristically from author/year/title:
the canonicalisation may pick a different disambiguating word, and the key may already exist
in the file under a different form.

### Long-running calls

Slow commands (rough thresholds):

| Command | Typical latency |
|---|---|
| `lit search --remote` | 2–10s |
| `lit refs`, `lit cites` | 3–15s |
| `lit path` | 5–30s |
| `lit add` (uncached) | 2–8s |
| `lit <id>` (uncached) | 1–5s |

Invoke these via shell with backgrounding when chaining multiple calls.
For more than three slow calls in one task, delegate to a background subagent
to keep raw API output out of the main context.

### No direct `.bib` edits

`lit` is the only writer for `.bib` files in this workspace.
Edit via `lit add`, `lit misc`, or `lit clean`.
Direct edits bypass canonicalisation, produce diffs that look wrong on the next `lit add`,
and create duplicate-entry races.

### No `WebFetch` for papers

`lit` is the boundary for paper retrieval. Use `lit download`, `lit read`,
or in-tree `etc/pdf/<id>.pdf` files. Parallel access paths defeat caching
and double the effective API quota.

---

## What this document deliberately omits

- Internal architecture: crate layout, DB schema, HTTP client design.
  See `src/` directly or add `ARCHITECTURE.md` if developer onboarding requires it.
- API endpoint specifics for each backend (OpenAlex, S2, CrossRef, ...).
  See `src/api/*.rs`.
- Test strategy. See `Makefile` and `test/lit.bats`.
