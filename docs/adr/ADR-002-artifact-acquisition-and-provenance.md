# ADR-002: Recorded facts over re-derivation in acquisition and provenance

## Status

Proposed.

## Context

This record covers one uncommitted change set spanning 19 files, 1415 insertions and 455 deletions.
It touches catalog sync, acquisition fallback, artifact provenance, failure typing, retry policy, BibTeX sanitization, TLS selection and the search flag surface.
Those look unrelated, but four of them are the same defect in different clothing, and it is worth stating the shared shape once rather than six times.

In each case `lit` knew a fact at one point in its execution, discarded it, and then re-derived a worse version of it later.
The catalog knew a record's DOI while parsing MARCXML, discarded it, and later re-searched an FTS5 index whose tokenizer makes exact DOI match unreliable.
An artifact writer knew the citekey it was writing for, discarded it, and `lit check` later guessed the citekey from the directory name.
The sanitize pass knew exactly which characters it rewrites, and `lit clean`'s linter re-derived that knowledge in a second hand-written scanner that disagreed with it.
`lit read` knew whether a paper was absent or merely unreadable, collapsed both into one opaque error, and callers re-derived "absent" from the failure and fired a spurious download.

The disagreement in the last case was observable, not theoretical.
Two consecutive `lit clean --apply` runs reported the identical three findings and printed "Sanitized 1 entry" both times while changing no bytes.
The linter flagged `\&amp;`, which the pass preserves on purpose, because an `&` preceded by a backslash never starts an HTML entity.
No number of runs could clear that finding.

## Decision

### A fact is recorded where it is known and read where it is needed

This is the general rule the specific decisions below instantiate.
A command that knows an identifier, a citekey, a source URL or a failure kind MUST persist it at that point.
A consumer MUST read the recorded value rather than reconstruct it from a weaker signal such as a filename, a full-text index or the absence of an error.

### Catalog: a DOI side table

`clio.db` gains `clio_doi(doi TEXT PRIMARY KEY, url TEXT NOT NULL, access INTEGER DEFAULT 0)`, written during `insert_batch` and read by `lookup_doi_url`.
FTS5 tokenization makes an exact DOI match through the full-text index unreliable, so exact lookup gets an exact index.

MARCXML parsing moves from deciding at each text event to accumulating a whole datafield and deciding at flush.
Only the second form can express a condition over several subfields, which is what `024 $a` qualified by `$2 == "doi"`, and `856 $u` qualified by `$7`, both require.
DOI extraction prefers MARC `024` with `$2 == "doi"` and falls back to an `856 $u` containing `doi.org/`.

### Acquisition: an explicit tier order

`lit download <doi>` resolves in the order open-access URL, then the local Clio index, then EZProxy.
The tier that actually produced the bytes is the artifact's provenance.

### Provenance: artifacts are self-describing

Artifact-producing commands MUST write to `source.yaml`:

- `bibtex_key`, when the artifact belongs to a bibliography entry
- `source_url`, when the artifact came from a URL
- every available stable identifier, including `doi`, `isbn` and `arxiv`

`lit attach <citekey> <bib_file> <pdf>` attaches a PDF to an entry that already exists, and MUST NOT create or modify any BibTeX entry.
It validates `%PDF` magic bytes before writing and refuses to overwrite an existing directory without `--force`.

`lit check --fix` MUST resolve metadata in this order:

1. Read `bibtex_key` from `source.yaml`.
2. If absent, use the artifact directory name as a candidate citekey against the bibliography supplied by `--bib-file`.
3. Merge missing metadata from the matching BibTeX entry.
4. If a stable identifier is available, use the identifier-specific lookup for the remaining fields.
5. Upsert the result and set the artifact's local path.

The operation MUST be idempotent: repeating it MUST NOT create duplicate papers or rewrite an already resolved artifact.
When no unambiguous citekey, title or stable identifier can be recovered, it MUST leave the artifact unchanged, emit a machine-readable unresolved record naming the path and the missing fields, and persist the set at `.lit/unresolved-artifacts.json`.
It MUST NOT invent an author, title or identifier.

### Sanitization: the linter is a function of the writer

`lit clean`'s findings MUST be derived from the sanitize pass, not from an independent scan.
The contract is that a finding exists exactly when `--apply` would change the field, implemented by testing `transform_value(value) == value` first and returning no findings when it holds.

This makes convergence structural rather than a property to be maintained by hand.
A linter that scans for defects on its own authority will eventually flag text the pass preserves deliberately, and that finding can never be cleared.
Idempotence is the design invariant that makes the deliberate preservation correct: each decoder strips one encoding level per pass, so `\&amp;` is a fixed point by construction and not an oversight.

`lit clean --apply` also MUST report an entry as fixed only when its bytes moved.
`replace_entry_block` succeeds for any key present in the file, so reporting on that alone claimed a fix on every run whether or not the pass changed anything.

Normalization covers punctuation only, through a 12-entry table from Unicode character to LaTeX spelling.
Accented letters in author names and Greek letters in titles are correct as written and MUST survive untouched.
Only a character that has a LaTeX spelling and arrived in Unicode form needs translating.
Three entries take the safe reading rather than the faithful one, because a metadata provider emits them as encoding artifacts more often than as typographic intent: U+00A0 becomes an ordinary space rather than a `~` tie, U+2011 loses its non-breaking sense, and U+2212 renders at hyphen width rather than as a `$-$` that would nest wrongly inside a title already in math mode.

### Failure typing: name the failure, do not infer it

`cmd::read` returns `ReadError::{NotFound, Unreadable{dir,message}, Other}`.
arXiv auto-download fires only on `NotFound`.
An unreadable artifact reports its real cause instead of the misleading "not found locally, download it first".

### Supporting policy changes

These are not instances of the rule above; they are recorded here because they ship in the same change set.

- HTTP retries drop from 60 attempts to 4, with `LIT_MAX_ATTEMPTS` as an override, and fixed 1s sleeps become `1 << min(attempt, 2)`, so 1s, 2s, 4s. Sixty attempts against a provider that is down is a hang, not a retry.
- TLS is rustls at every `Client::builder()` site, and `Cargo.toml` sets `default-features = false` with `rustls-tls-webpki-roots`. macOS SecureTransport needs keychain access and fails with OSStatus -26276 under a seatbelt sandbox, so the roots are bundled rather than read from the system keychain.
- `lit search` inverts its flag polarity: `--remote` is gone and `--local` restricts to the downloaded corpus, making remote the default.
- `find_main_tex` accepts `\documentstyle`, so LaTeX 2.09 sources resolve.

## Alternatives rejected

### Infer artifact metadata from the PDF filename

Filenames are abbreviated, duplicated or local working names.
They are usable as citekey candidates, which is what step 2 above does, and not as bibliographic evidence.

### Register every artifact as `unknown`

The database then looks consistent while identity is destroyed, which makes later deduplication unsafe.

### Require manual database edits

That bypasses canonicalization and collision checks and violates the single-writer rule.

### Fix the three `lit clean` non-convergence bugs individually

Each was a separate disagreement between the linter and the pass.
Fixing them one at a time leaves the second scanner in place and guarantees the fourth disagreement.
Deriving the findings from the pass makes disagreement impossible rather than merely absent today.

## Consequences

New artifacts become self-describing and reconcile automatically after a database rebuild or a checkout migration.
Existing artifacts with a recoverable directory-to-BibTeX mapping become repairable without hand-editing SQLite.
Artifacts with genuinely insufficient evidence stay visible as explicit work items instead of disappearing into a warning stream.
`lit clean` converges, which it previously did not.

### Known limitations in this change set

Three independent reviews ran over the diff, split by area.
The findings below were confirmed against the source; the ones marked "verified here" were additionally re-read directly rather than taken from the review.

| ID | Limitation | Location |
|----|-----------|----------|
| L1 | Neither `clear_clio_db` nor `clio sync --force` deletes from `clio_doi`, and inserts are `INSERT OR IGNORE`, so a stale DOI-to-URL row survives every re-sync and permanently shadows a corrected URL. `docs/CLIO.md:147` claims force "clears index and re-syncs unconditionally". Verified here. | `src/api/clio.rs:162-165`, `:191`; `src/cmd/clio.rs:139-146` |
| L2 | An existing `clio.db` can never populate `clio_doi` without `--force`, because sync skips files already recorded in `clio_meta`. On any machine with a completed prior sync the new lookup returns `None` forever with no message. | `src/api/clio.rs` sync path |
| L3 | `source_url` records the first tier that had a candidate URL, not the tier that delivered the bytes. When an open-access URL exists but its fetch fails and EZProxy succeeds, the artifact claims the open-access URL as its source. This contradicts the tier decision above. Verified here. | `src/cmd/download.rs:158` |
| L4 | A dead open-access link disables the Clio tier but not the EZProxy tier, because the Clio branch is gated on `pdf_url.is_none()`. | `src/cmd/download.rs:100` |
| L5 | `source.yaml` gets two `bibtex_key` lines: `build_misc_source_yaml` writes one and `attach_pdf_data` appends another. A strict YAML parser rejects duplicate keys, and the two readers in `check.rs` disagree, one taking the first and one the last. Verified here. | `src/cmd/misc.rs:173` vs `:208` |
| L6 | `attach` substitutes `title: "unknown"`, `authors: "unknown"` and `year: "?"` when the entry lacks them, and persists those to `source.yaml`. This violates the MUST NOT invent clause above. `merge_bib_fallback` only overrides an author list that is exactly `"[]"`, so the fabricated value survives into `upsert_paper`. Verified here. | `src/cmd/misc.rs:137-145`; `src/cmd/check.rs:59` |
| L7 | The unresolved report is lost on any error. A `?` in the loop returns before the write, after database rows have already committed, leaving filesystem and database inconsistent. Verified here. | `src/cmd/check.rs:262`, `:290`, `:306-312` |
| L8 | `upsert_paper` and `set_local_path` are two unsynchronized statements with no transaction. A failure between them leaves a paper row with no `local_path`, which the next run re-upserts as a fresh insert for an identifier-less artifact. | `src/cmd/check.rs:290-291` |
| L9 | `set_local_path` overwrites unconditionally, so two artifact directories sharing one DOI flip the same row's pointer on alternating runs, each run reporting the other as missing. This is a direct idempotence violation. | `src/cmd/check.rs:290-291`; `src/db.rs:665-672` |
| L10 | `strip_prefix(&root).unwrap_or(&dir_path)` stores an absolute path when the artifact is not under the computed root, and `project_root()` is cwd-dependent, so the dedup key is unstable across invocation directories. | `src/cmd/check.rs:251-255`, `:103-121` |
| L11 | Under `--json`, prose status lines print to stdout alongside the JSON object, so the output is not machine-parseable. Verified here. | `src/cmd/check.rs:298-316` |
| L12 | Ambiguity is undefined in the implementation. `entries.iter().find(...)` takes the first match and never detects duplicate keys in the bibliography. | `src/cmd/check.rs:267` |
| L13 | `isbn` is written by `attach` but never parsed by `parse_source_yaml` and never used by the identifier fallback, so books never resolve through `check --fix`. | `src/cmd/check.rs:158-172`, `:71-87` |
| L14 | The identifier lookup fires only when the title is still `unknown`, so a titled artifact with a bare DOI never gets its remaining fields filled. Widening it as written above costs one network call per artifact. | `src/cmd/check.rs:271` |
| L15 | `attach --force` overwrites `paper.pdf` and `source.yaml` but leaves the previous `paper.txt`, because `ensure_text` returns early when a non-empty file exists. The extracted text keeps describing the old PDF. | `src/cmd/misc.rs:167`; `src/cmd/read.rs:143-153` |
| L16 | `attach --force` skips rollback: `dir_existed` is recomputed after the download and is `true` in the overwrite case, so a failed write leaves a half-written directory with no warning. Verified here. | `src/cmd/misc.rs:155`, `:166-172` |
| L17 | `.lit/unresolved-artifacts.json` is newly generated project-local state and is absent from `.gitignore`, so a `--fix` run dirties the tree. Verified here. | `src/cmd/check.rs:307-312`; `.gitignore` |
| L18 | `src/config.rs` is not declared in `src/lib.rs`, so `config::get()` is never compiled or called and its tests never run. The new "Where state lives" section of `DESIGN.md` is normative about machinery no compiled code uses, and asserts every claim is covered by a test. Verified here. | `src/lib.rs`; `docs/DESIGN.md:147-149` |
| L19 | Three MUST invariants in `DESIGN.md` are violated by current code: a default derived from `current_exe()`, no `create_dir_all` before opening the database, and no `lit db path` subcommand. | `docs/DESIGN.md:277-291`; `src/main.rs:274-283`; `src/db.rs:40-41` |
| L20 | `lit search --local` is silently overridden by `--source`, because `use_remote = !local \|\| source.is_some()`. | `src/main.rs:334` |
| L21 | `lit attach` is documented nowhere: absent from `README.md`, `docs/DESIGN.md`, `docs/WORKFLOWS.md` and the lit skill. | `src/main.rs:218` |

### Required tests, not yet written

The sanitize decision is covered: ten tests, including one asserting that findings are empty exactly when the pass is a no-op, which is the convergence contract stated as an executable oracle.

The provenance decision is not.
`attach_pdf_data` has no integration test, and `tests/` contains no `check` file at all.
The following are required before this record moves to Accepted:

- Idempotence: `check --fix` twice over a fixture tree leaves paper count, `local_path` values and report file byte-identical.
- Duplicate creation: two artifacts sharing a DOI, and two runs from different working directories, exercising L9 and L10.
- Resolution order: a `bibtex_key` disagreeing with the directory name must win; the directory name must be used only when `bibtex_key` is absent.
- Ambiguity: duplicate keys in the bibliography must reach the unresolved report rather than silently taking the first.
- Fabrication: an entry lacking author or title must make `attach` fail rather than write `unknown`, which is L6.
- Unresolved report: created with the right shape, deleted when the set empties, and preserved when a later artifact raises an error, which is L7.

`fill_identifier_fallback` constructs its client inline, which is the structural reason the reconciler cannot be tested end to end.
Testing it requires injecting the client.

## Evidence

Convergence was measured before the fix, not inferred: two consecutive `lit clean --apply` runs on the same file reported the same three findings and printed "Sanitized 1 entry" both times while changing no bytes.

The release build passes and 383 tests pass.
One bats test, `auto-detect: ISBN` at `test/lit.bats:48`, fails against an Open Library lookup that returns HTTP 404 for the queried ISBN.
That failure is provider-side and predates this change set.
