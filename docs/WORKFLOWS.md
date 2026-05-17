# lit — Workflows

Worked examples for the common tasks `lit` is built around.
For *what* each command does, see [`../README.md`](../README.md).
For *why* the API is shaped this way, see [`DESIGN.md`](DESIGN.md).

---

## 1. Cite a new paper while writing

You just read a paper and want to `\cite{}` it in a LaTeX section.

```bash
lit "denoising diffusion probabilistic models"     # search local DB
lit 2006.11239                                     # confirm metadata + abstract
lit add 2006.11239 ext/Improving-Causal-Explanations/references.bib --json
# → {"entry_key": "ho2020denoising", "bib_file": ".../references.bib"}
```

Use `ho2020denoising` in `\cite{}`. Never invent the key — the canonical form
may disambiguate with a different word than you'd guess.

If the local search miss is a true miss (not just an index gap), try `--remote`:

```bash
lit search --remote "denoising diffusion probabilistic models" --limit 5
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

## 4. Find related work via the citation graph

```bash
lit refs 2006.11239 --json   # what this paper builds on
lit cites 2006.11239 --json  # what built on this paper
```

Both are slow (one to ten seconds, depending on source). When chaining several
or scanning a graph neighbourhood, invoke via Bash with `run_in_background: true`,
or hand off to a background subagent so raw API output stays out of the main
context.

For deeper exploration, increase `--hops`:

```bash
lit refs 2006.11239 --hops 2 --max-papers 200
```

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
# → /abs/path/to/etc/pdf/2006.11239/text.md
```

The output is a path — use shell substitution to feed it into another command:

```bash
cat "$(lit read 2006.11239)"
grep -i "diffusion" "$(lit read 2006.11239)"
```

If the paper isn't cached and the ID looks like arXiv, `lit read` auto-downloads
the PDF, extracts text, then returns the path. Behaviour mirrors the historical
MCP handler.

For JSON output (e.g. when calling from an agent):

```bash
lit read 2006.11239 --json
# → {"path": "...", "format": "markdown", "extra_files": [...]}
```

---

## 8. Survey a topic: local vs. remote search

Local DB search is instant (full-text search over previously-fetched metadata):

```bash
lit search "shapley xai"
```

Remote search hits APIs (slow, but fresh + comprehensive):

```bash
lit search --remote "shapley xai" --source ss --limit 20
lit search --remote "shapley xai" --source all --limit 50   # merge all sources
```

`--source` is the lever for recall vs. precision vs. latency:
- `oa` (default) — broad, fast.
- `ss` — best citation-graph quality.
- `cr` — DOI-authoritative; canonical for journals.
- `dblp` — CS conferences OA tends to miss.
- `philpapers` — philosophy coverage.
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

# Rebuild DB from filesystem (etc/pdf/**/source.yaml)
lit db rebuild

# Nuclear option (rare): delete the cache directory entirely
rm -rf "${LIT_CACHE_DIR:-etc/lit/cache}"
```

`lit db rebuild` is the right first move — it reconstructs the SQLite database
from on-disk source-of-truth files. Only delete the cache dir if `rebuild` also
fails to recover.

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
