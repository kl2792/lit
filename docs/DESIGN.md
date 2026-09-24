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
| `lit search <query>` | Ranked list | OpenAlex by default; `--local` restricts to downloaded papers |

Two commands, not one with a `--mode` flag.
Lookup is identity-keyed (deterministic input → single result, cache hit rate near 1.0).
Search is exploratory (query → ranked list, no identity).
Collapsing them would force lookup callers to unwrap single-element lists,
and force search callers to wrap single inputs into lists.
They also have disjoint flag surfaces (`--limit`, `--source`, `--local` apply only to search).

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

`download` deposits a file. `read` returns the path to extracted text.
Different cost, different output shape.
The name is a compromise: `read` locates and extracts, the caller reads.
`path` was unavailable, being taken by citation distance.

`download` is not part of `lookup` because PDFs are bandwidth-heavy and most lookups don't
need full text — they need to confirm a citekey, fetch BibTeX, or check the abstract.
Auto-downloading would burn disk and bandwidth on every metadata query.

`read` exists as a separate command (rather than `pdftotext` over the cached file) because:
- It hits cached extracted text; re-extracting from PDF is expensive.
- It has an auto-download fallback for arXiv IDs: if the paper isn't cached, fetch it first.
- The format is normalized: LaTeX source when the artifact has it, extracted text otherwise.

### Bib management: `add`, `misc`, `attach`, `verify`, `clean`

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

`attach` files a PDF against an entry that already exists.
It is separate from `misc --pdf` because it is the case where the bibliography is already
right: the only missing thing is the artifact.
Folding it into `misc` would mean the one command that never touches BibTeX shares a code
path with the one whose purpose is to write BibTeX, and the flag distinguishing them would
carry the whole contract.
What it does write is provenance: the citekey it was asked for, where the bytes came from,
and the entry's stable identifiers, which is what lets `check --fix` reconcile the artifact
later without guessing from the directory name.

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

`check --fix` is provenance-first.
Artifact writers record a `bibtex_key` and stable identifiers when available.
They also record `metadata_confirmed` when a metadata provider answered for the work's identifier, which is what the checker reads to decide whether a lookup is still owed.
When an artifact is not indexed, the checker may use an explicitly supplied
bibliography (`--bib-file`) to fill missing fields for that citekey before upserting it.
It never invents metadata; unresolved artifacts remain unchanged and are emitted
as structured records in JSON mode and persisted to `.lit/unresolved-artifacts.json`.
Decided by [ADR-002](adr/ADR-002-artifact-acquisition-and-provenance.md).

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
No record decides this rule; it is listed as an open gap in [the log](adr/README.md).

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
  "format": "txt",           // "tex" | "txt" | "txt (generated from PDF)"
  "extra_files": ["..."]     // sibling .tex files, when the source is LaTeX
}
```

Emitted by `main.rs` under `--json` and by the MCP `read` handler.
The MCP handler omits `extra_files`.

### `lit verify` / `lit clean`

Status lines per entry; exit nonzero on any failure.
See README examples.

---

## Cache and rate-limit behavior

- **Location:** a table inside the SQLite database, so the cache moves with the library and
  cannot diverge from it. See "Where state lives" below for how that path resolves.
- **TTL:** 24 hours for search results, 7 days for identifier lookups.
- **Invalidation:** `--no-cache` bypasses on a single call.
  No "clear cache" subcommand by design; `lit db rebuild` reconstructs the database from
  `etc/pdf/**/source.yaml` if you need a full reset.
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
| `lit search` (remote, the default) | 2–10s |
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

## Where state lives

This section is normative and restates
[ADR-003](adr/ADR-003-where-lit-keeps-its-state.md), which decides every invariant below
except where another record or an open gap is named.

### The paths

Each path resolves in three steps: the environment variable if it is set, otherwise the
stated default, otherwise an error naming the variable that would fix it.
One resolver per path, and no configuration file.
Paths that do not yet meet this are listed under "Open gaps" below.

| What | Environment | Default | Resolved by |
|---|---|---|---|
| Database, including the response cache | `LIT_DB_PATH` | `etc/lit/lit.db` under the executable's grandparent directory | `paths.rs`, called by both binaries |
| Project root | `LIT_PROJECT_ROOT` | nearest ancestor of the working directory holding a non-empty `etc/pdf/` | `paths.rs` |
| PDF artifacts | *(via the project root)* | `etc/pdf/` under the project root | `paths.rs`; `cmd/read.rs` and `cmd/download.rs` both call `paths::artifact_dir()` |
| Clio catalog index | `LIT_CLIO_DB_PATH` | nearest `etc/lit/` at or above the working directory | `api/clio.rs` |
| EZProxy cookies | *(none)* | nearest `.cache/lit/clio/cookies.txt` at or above the working directory | `cmd/download.rs` |
| Unresolved-artifact report | *(via the project root)* | `.lit/unresolved-artifacts.json` under the project root | `cmd/check.rs` |

`lit db path` prints each resolved value with the source that set it.

### Invariants

- A path MUST NOT fall back to the working directory or to a bare relative path.
  A scan rooted at the wrong place finds no artifacts and reports what an intact library
  reports, so the fallback makes a misconfiguration indistinguishable from a pass.
  Held by `paths::tests::project_root_without_artifacts_is_an_error`.
- An empty `etc/pdf/` MUST NOT satisfy the project-root search.
  A fresh clone and a checkout whose artifacts were never fetched both present one.
  Held by `paths::tests::empty_artifact_dir_does_not_count_as_a_root`.
- A default MUST NOT be derived from the location of the executable.
  Where a binary is installed is not a statement about where a user's library lives.
  Deriving one from the other means `make install` silently repoints the library, and
  `make clean` silently deletes it.
  The database default deviates from this; ADR-003 carries the deviation deliberately and
  leaves the replacement open, because changing it repoints every existing installation.
- No user-generated state may live under `target/`.
  `make clean` runs `cargo clean`.
- `lit` MUST create the parent directory of the database before opening it.
  Omitting this yields `Failed to open database: failed to open database`, which names
  neither the path nor the cause, because the anyhow context masks the SQLite error.
  Held by `db::tests::test_open_creates_missing_parent_directory`.
- `lit db path` MUST print every resolved path together with the source that won, so a
  misconfiguration is diagnosable without reading the source.
  It runs before the database is opened, because an unopenable database is the case it
  exists to diagnose.
- Every environment variable this document names MUST be read by the code.
  `LIT_CACHE_DIR` was documented in three files and read by none.
  No record decides this rule; it is listed as an open gap in [the log](adr/README.md).

### Open gaps

- The Clio index default and the EZProxy cookie path are both working-directory dependent,
  and the cookie path has no override.
  ADR-003 records both as unruled.

---

## What belongs in this document

Between 17 May and 23 August 2026 this document drifted from the code in exactly four
places, and every one was mechanical: an environment variable no source file reads
(`LIT_CACHE_DIR`), a flag name that inverted (`--remote` became `--local`), a JSON contract
never implemented (`lit read`), and a one-line description contradicted by the command's own
help text.

Every argument in "Command rationale" was still accurate.
The latency case for keeping `refs` and `cites` apart, the case for `add` as a stateful
merge rather than a shell redirection, the cadence distinction between `clean` and `verify`:
all still true, all unchanged.

The split is not luck.
A fact about the surface is a copy of something the code already states, and copies drift.
An argument is a copy of nothing.

This yields the rule, which is the existing "don't repeat yourself" principle applied across
the boundary between prose and source:

- **Generated, never hand-written.** The command list, flag names, and environment variable
  names. `clap` already holds them. `README.md`'s tables should be generated, with a test
  that fails when they go stale.
- **Tested, named here but not valued here.** JSON output contracts and path resolution.
  This document states the decision and names the test; the test holds the value.
- **Hand-written, never generated.** Rationale, rejected alternatives, and the reason behind
  each invariant. This is what the document is for, and no generator can produce it.

A claim in prose that a machine could check is a defect waiting to be filed.

This is also why the system should not be generated *from* this document.
Anything sufficient to generate the implementation would have to contain the implementation,
at which point the generator input is the real source and the problem has only moved.
The achievable version is the inverse: the document stops asserting what the code already
knows, and asserts only what the code cannot.

---

## Commands not yet built

### People: `author`

Every command above is keyed on a work: an identifier, a title, a query, or a citekey.
None is keyed on a person.
`PaperResult` carries `authors` as bare display strings with no identifier, and the only
code reading them extracts a surname for citekey generation.

This is a gap rather than a decision.
Answering what a person has published, who they work with, or whether two people have ever
coauthored currently requires querying OpenAlex by hand, which defeats the cache, the rate
limiter, and the "CLI is the boundary" principle in one step.

`lit author <name>` resolves a name to an OpenAlex author identifier and lists works with
coauthors.
The identifier is the point, because two researchers share a name and only the id separates
them.

One hazard the output must surface: OpenAlex merges records.
A single entry can fold two distinct works together, and the absorbed work's coauthors then
vanish without any indication.
Where an entry's locations disagree about venue, `lit author` must say so rather than
present the merge as one work.
No record decides either requirement, and nothing checks them while the command is unbuilt;
both are listed as an open gap in [the log](adr/README.md).

### Naming the unnamed: `lookup`

Principle 2 says identity and discovery get separate commands, and the table above lists
`lit <id>` as a command in its own right.
The CLI never names it; it is the bare positional fallthrough.
The MCP surface does name it, as `lookup`.

Two surfaces disagreeing about whether a command exists is precisely the drift this document
exists to prevent, so the CLI takes the same name and bare input becomes its alias.

---

## What this document deliberately omits

- Internal architecture: crate layout, DB schema, HTTP client design.
  See `src/` directly or add `ARCHITECTURE.md` if developer onboarding requires it.
- API endpoint specifics for each backend (OpenAlex, S2, CrossRef, ...).
  See `src/api/*.rs`.
- Test strategy. See `Makefile` and `test/lit.bats`.
