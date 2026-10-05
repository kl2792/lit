# lit — Workflows

Worked examples for the common tasks `lit` is built around.
For *what* each command does, see [`../README.md`](../README.md).
For *why* the API is shaped this way, see [`DESIGN.md`](DESIGN.md).

---

## 1. Cite a new paper while writing

You just read a paper and want to `\cite{}` it in a LaTeX section.

```bash
lit "denoising diffusion probabilistic models"     # search (remote APIs by default)
lit 2006.11239                                     # confirm metadata + abstract
lit add 2006.11239 ext/Improving-Causal-Explanations/references.bib --json
# → {"entry_key": "ho2020denoising", "bib_file": ".../references.bib"}
```

Use `ho2020denoising` in `\cite{}`. Never invent the key — the canonical form
may disambiguate with a different word than you'd guess.

To restrict the search to papers you have already downloaded (instant, no API
calls), use `--local`:

```bash
lit search --local "denoising diffusion probabilistic models" --limit 5
```

---

## 2. Cite a textbook or work with no DOI/arXiv ID

`add` can't help — there's no resolvable identifier. Use `misc`:

```bash
lit misc halpern2016actual \
  ext/Improving-Causal-Explanations/references.bib \
  -t "Actual Causality" \
  -y 2016 \
  -a "Joseph Y. Halpern" \
  --howpublished "MIT Press"
# → Added @misc{halpern2016actual} to .../references.bib
```

Repeat `-a` per author. `--howpublished` is the free-text venue; `--note` adds
an arbitrary annotation field.

For tech reports and working papers where you also have the PDF (a local file
or a URL), add `--pdf`: it creates `etc/pdf/<citekey>/` with `paper.pdf`,
`source.yaml`, and extracted `paper.txt`, then writes the bib entry. This is
the documented ingestion path for identifier-less artifacts.

```bash
lit misc maiti2026tier refs.bib \
  -t "Counterfactual Tiers Tech Report" \
  -y 2026 \
  -a "Aurghya Maiti" \
  --howpublished "Tech report R-125" \
  --pdf ~/Downloads/r125.pdf        # or --pdf https://causalai.net/r125.pdf
```

On download or validation failure nothing is written (no directory, no bib
entry) and the command exits nonzero with a hint — for sandbox-blocked hosts,
download in a browser and rerun with the local path. An existing
`etc/pdf/<citekey>/` directory is an error unless you pass `--force`.

---

## 2b. Attach a PDF to an entry you already have

The bibliography entry is already correct and you have just obtained the PDF.
`lit misc --pdf` is the wrong tool here: it would write a second entry.
Use `attach`, which never creates or modifies BibTeX.

```bash
lit attach halpern2016actual refs.bib ~/Downloads/actual-causality.pdf
lit attach halpern2016actual refs.bib https://example.org/actual-causality.pdf
```

It fails if `refs.bib` has no entry under that citekey, and if the bytes do not
begin with `%PDF`, so a login page saved as a `.pdf` is caught rather than
filed. On success it writes `etc/pdf/<citekey>/` with `paper.pdf`,
`source.yaml`, and extracted `paper.txt`, recording the citekey and the source
in `source.yaml` so `lit check --fix` can reconcile the artifact into the
database later. An existing directory is an error unless you pass `--force`.

---

## 3. Stub a forthcoming preprint

You need to cite a paper that doesn't have an arXiv ID yet (e.g. waiting for
co-author sign-off). Stub it with `misc` and a note; replace with `lit add`
later when the arXiv ID lands.

```bash
lit misc lee2025counterfactual mech/references.bib \
  -t "Improving Causal Explanations" \
  -y 2025 \
  -a "Kai-Zhan Lee" -a "Elias Bareinboim" \
  --note "Preprint forthcoming"
```

Once on arXiv, delete the misc entry by hand or via `lit clean --apply` after
adding the real one with `lit add`.

---

## 3b. Update an entry (preprint → conference, metadata fix)

`lit add` is upsert: it overwrites an existing entry with the same key.
For a true refresh (bypassing cache to re-fetch from the source API), use `--no-cache`:

```bash
lit add --no-cache 2006.11239 references.bib --json
```

There is no separate `lit update` command by design — `add --no-cache` is the same
operation. If the citekey itself needs to change (e.g. `ho2020denoising` →
`ho2020ddpm` because a new version disambiguates differently), remove the old
entry first:

```bash
lit remove ho2020denoising references.bib
lit add 2006.11239 references.bib --json   # canonical key returned in entry_key
```

---

## 3c. Citekey collisions

Two different papers can canonicalize to the same citekey (same first-author
surname, year, and leading title word). `lit add` and `lit misc` abort instead
of silently overwriting when an upsert would replace an entry whose title
differs materially, showing both titles. Disambiguate with `--key` on `add`
(pick a different citekey for `misc`), or pass `--force` to overwrite
deliberately:

```bash
lit add 10.1609/aaai.v39i25.34888 refs.bib --key maiti2025aaai --json
```

---

## 4. Find related work via the citation graph

```bash
lit refs 2006.11239 --json   # what this paper builds on
lit cites 2006.11239 --json  # what built on this paper
```

Each lists every neighbor: Semantic Scholar is paged to the end.
If any Semantic Scholar call fails (a 429, say), the list comes from OpenAlex instead, which needs a DOI or arXiv id.
Each `--json` record carries `doi`, `arxiv_id`, `s2_id` where known, `source` (`s2` or `openalex`), and `abstract` (full text or null; `lit search --json` records carry it too).
A failed call exits nonzero; on a deeper hop the partial results print first and the failures go to stderr.

Both are slow (one to ten seconds, depending on source). When chaining several
or scanning a graph neighbourhood, invoke via Bash with `run_in_background: true`,
or hand off to a background subagent so raw API output stays out of the main
context.

For deeper exploration, increase `--hops`:

```bash
lit refs 2006.11239 --hops 2 --max-papers 200
```

### Closure over several seeds: `lit closure`

```bash
lit closure 2408.01416 10.1234/x --hops 1 --direction both \
    --exclude-bib refs.bib --expand-only expand.txt --json
```

`lit closure` runs the whole BFS in one call, with its neighbor calls made concurrently inside the binary, so do not chain `lit refs` / `lit cites` by hand.
Seeds are DOIs or arXiv ids, given as arguments or in `--seeds-file` (one per line, `#` starts a comment).
Seeds are always expanded; a paper at hop 1 or deeper is expanded only if `--expand-only` is absent or lists its `id_key`.
Each paper appears once, under `id_key` = `doi:<lowercased DOI>`, else `arxiv:<id without version>`, else `title:<lowercase alphanumerics and single spaces>`.
Records sharing a DOI or arXiv id merge, so a record carrying both joins records keyed by either; a shared title merges only when no DOI or arXiv id contradicts it.
`--exclude-bib` (repeatable) marks a paper `known` with the matching citekey and file when a .bib entry shares its DOI, `eprint` arXiv id, or normalized title; known papers stay in the output.
For a paper with no DOI or arXiv id, a .bib title also matches when its words appear in order inside the paper's title, or the reverse, provided the shorter title has at least 4 words and at least half as many as the longer; this catches reference-string titles such as "2023 Towards Monosemanticity: ...".
`--max-papers` stops adding papers, with a warning on stderr; by default there is no cap.

Output is JSONL in this order, with or without `--json`:
1. `{"type":"run","lit_version","timestamp","argv","seeds","hops","direction"}`.
2. One `{"type":"paper","id_key","doi","arxiv_id","s2_id","title","authors","year","venue","abstract","hop","edges":[{"kind","from"}],"known","source"}` per paper, by hop. `abstract` is the full text or null. Seeds are hop-0 records; a seed has a title and abstract only if another seed's neighbor list reaches it. An edge `{"kind":"refs","from":X}` means the paper is in X's references; `"cites"` means it cites X.
3. One `{"type":"error","call","id","message"}` per neighbor call that failed on both Semantic Scholar and OpenAlex. A call that succeeds through the OpenAlex fallback is not an error; its papers carry `"source":"openalex"`.
4. `{"type":"summary","papers","known","errors"}`.

The exit status is 0 if at least one neighbor call succeeded.
Papers with no DOI, arXiv id, or title are dropped and counted on stderr.

---

## 5. Connect two papers through the citation graph

Useful for "did A influence B" questions or finding bridge papers in a literature
review.

```bash
lit path 2006.11239 1810.04805 --max-hops 5
# → {"path": ["paper_a_id", "...", "paper_b_id"], "hops": 3}
```

This is a server-side BFS — much faster than repeated `refs`/`cites` calls.

---

## 6. Pre-submission bib hygiene

Run before every paper compile near the deadline:

```bash
# Local checks (fast): malformed entries, dupes, orphans
lit clean ext/Improving-Causal-Explanations/references.bib \
  --tex ext/Improving-Causal-Explanations/sections/

# Apply the safe fixes (remove malformed + dupes)
lit clean ... --apply

# Also prune orphans (entries not cited anywhere in --tex dirs)
lit clean ... --apply --prune --tex sections/

# Online check (slow): re-resolve every entry against APIs
lit verify ext/Improving-Causal-Explanations/references.bib -j 8
```

`clean` runs on every commit; `verify` runs monthly or before submission.

---

## 7. Read a cached paper's text

```bash
lit read 2006.11239
# → /abs/path/to/etc/pdf/2006.11239/paper.txt
```

The output is a path — use shell substitution to feed it into another command:

```bash
cat "$(lit read 2006.11239)"
grep -i "diffusion" "$(lit read 2006.11239)"
```

If the paper isn't cached and the ID looks like arXiv, `lit read` auto-downloads
the PDF, extracts text, then returns the path. Behaviour mirrors the historical
MCP handler.
When the arXiv API is rate-limiting, metadata comes from Semantic Scholar or OpenAlex (a one-line note names which), and if none answers the download still proceeds into `etc/pdf/<arxiv id>/` (ADR-004).

For JSON output (e.g. when calling from an agent):

```bash
lit read 2006.11239 --json
# → {"path": "...", "format": "txt (generated from PDF)", "extra_files": []}
```

Pass several ids to read them in one command instead of a shell loop:

```bash
lit read 2006.11239 pearl2009causality halpern2016actual
# → one path line per id, in argument order
lit read 2006.11239 pearl2009causality --json
# → [{"path": ...}, {"path": ...}]
```

Ids run sequentially, so arXiv auto-downloads run one at a time under the usual HTTP retry budget.
A failed id prints `<id>: <error>` to stderr and the remaining ids still run; the exit code is 1 if any id failed, and `--json` prints the array of the successful objects.
With a single id, output and errors are exactly the single-id form above, including a bare object under `--json`.

---

## 8. Survey a topic: local vs. remote search

Remote search is the default and hits APIs (slow, but fresh + comprehensive):

```bash
lit search "shapley xai" --source ss --limit 20
lit search "shapley xai" --source all --limit 50   # merge all sources
```

Local DB search is instant (full-text search over previously-fetched
metadata) but only covers papers you have already downloaded:

```bash
lit search --local "shapley xai"
```

`--local` and `--source` are mutually exclusive: `--source` names a remote
backend, so the pair has no meaning and `lit` rejects it rather than picking one.

`--source` is the lever for recall vs. precision vs. latency:
- `oa` (default) — broad, fast.
- `ss` — best citation-graph quality.
- `cr` — DOI-authoritative; canonical for journals.
- `dblp` — CS conferences OA tends to miss.
- `philpapers` — philosophy coverage.
- `clio` — Columbia catalog (local index, requires `lit clio sync`).
- `all` — merge across sources.

For **more than three** remote searches in one task, delegate to a background
subagent. The raw API output is bulky and shouldn't sit in main context.

---

## 9. Share entries across multiple papers

This workspace has several papers that overlap in references
(e.g. `ext/Improving-Causal-Explanations/references.bib` and
`mech/references.bib` both cite causal-inference foundations).

There is **no shared `.bib` mechanism in lit** — each paper has its own file,
and the same paper can end up under different citekeys in each
(canonicalisation is deterministic but depends on local collisions).

Patterns that work:

```bash
# Blessed multi-add idiom: loop the adds, verify once at the end.
# add is upsert-idempotent, so retries are safe; the collision guard makes
# the loop fail loudly if two papers canonicalize to the same citekey.
for id in 2006.11239 1810.04805 10.1145/3442188.3445899; do
  lit add "$id" refs.bib --json
done
lit verify refs.bib -j 8
```

```bash
# Add a paper to multiple bibs in one go
for bib in ext/Improving-Causal-Explanations/references.bib mech/references.bib; do
  lit add 2006.11239 "$bib" --json
done
```

```bash
# Audit which papers appear in both (after the fact)
comm -12 \
  <(grep -oE '@[a-z]+\{[^,]+' a.bib | sort -u) \
  <(grep -oE '@[a-z]+\{[^,]+' b.bib | sort -u)
```

```bash
# Cross-check the citekey lit assigns is stable across files
lit add 2006.11239 a.bib --json | jq -r .entry_key
lit add 2006.11239 b.bib --json | jq -r .entry_key
# These should match — if not, a citekey collision exists in one of the files
```

If you need bib-level deduplication or a single shared bib, that's outside
lit's scope today; the closest workaround is one canonical `references.bib`
symlinked into each paper directory.

---

## 10. Recover from a broken cache or DB

Symptoms: stale results, unexpected errors from `lit search`, `lit refs` returning
empty when papers are known cached.

```bash
# Single-call escape hatch
lit --no-cache <id>

# Inspect what's there
lit db stats

# Show every resolved state path and what set it
lit db path

# Rebuild DB from filesystem (etc/pdf/**/source.yaml)
lit db rebuild
```

`lit db path` first: a "missing" paper is usually a database in a different
place from the one you expect, which `LIT_DB_PATH` sets and the executable's
location otherwise determines.
`LIT_PROJECT_ROOT` sets the artifact library that `check` scans, which is
otherwise the nearest directory above you holding a non-empty `etc/pdf/`.
`lit db rebuild` reconstructs the SQLite database, cache included, from the
on-disk source-of-truth files, so there is no separate cache to delete.

---

## 11. Migrate from MCP-era patterns

Old (MCP tool call):
```
mcp__lit__add(input="2006.11239", bib_file="references.bib")
# returns {"entry_key": "...", "bib_file": "..."}
```

New (CLI):
```bash
lit add 2006.11239 references.bib --json
```

Old:
```
mcp__lit__misc(citekey="halpern2016actual", title="Actual Causality",
               authors=["Joseph Y. Halpern"], year="2016",
               bib_file="references.bib")
```

New:
```bash
lit misc halpern2016actual references.bib \
  -t "Actual Causality" -y 2016 -a "Joseph Y. Halpern"
```

The MCP returned `task_id` for slow tools and pushed completion notifications.
The CLI is synchronous — use Bash `run_in_background: true` for chaining, or
delegate to a subagent for bulk work.

---

## Anti-patterns

- **Hand-editing `.bib` files.** Always go through `lit add`, `lit misc`, or `lit clean`.
  Direct edits bypass canonicalisation and create duplicate-entry races.
- **Guessing citekeys.** Always use `entry_key` from `--json` output of `add`/`misc`.
- **`WebFetch` for papers.** Use `lit download` and `lit read`. One cache, one rate limiter.
- **Sequential slow calls in the main context.** Use background invocation or subagents
  past three slow operations.
