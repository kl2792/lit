# ADR-011: Free-text search falls back to OpenAlex and arXiv together, ranks by topic words, and never reports an outage as zero hits

## Status

Proposed.

## Context

On 2026-11-13 about 22 free-text `lit search` queries, sent without `S2_API_KEY`, each failed on Semantic Scholar with HTTP 429 after 5 attempts.
The cascade then answered with noise; for "dropout Shapley value hidden units" it printed ten game-theory book chapters from CrossRef ("Power and the Shapley Value", "A value for n-person games", ...).

The noise had three causes, none of them an OpenAlex query parameter.
`openalex::search_url` already sends `search=`, which OpenAlex ranks by relevance over title, abstract and full text, identical to `filter=default.search:`; `filter=title.search:` returned 0 works for that query.

1. The cascade stopped at the first source whose own first result shared 30% of the query's tokens with its title and authors.
   Abstracts were ignored and stopwords counted, so OpenAlex's first result scored 20% and was skipped, and CrossRef, which matches titles and has no abstracts, scored 40% on "Shapley value" and was returned alone, discarding OpenAlex.
2. The final ranking re-sorted by the same title overlap, then by citation count, which ranks highly cited general works above on-topic ones.
3. No fallback source was built for free text: OpenAlex `search=` matches full text, so its top results for that query were Shapley-value papers about other topics.

Semantic Scholar sent no Retry-After, so each query spent about 40s in backoff (2, 5.7, 8.7, 23.8s) before any fallback.
A failed source was printed as a note and treated as empty, so a query on which every source failed printed "No results found" and exited 0.

OpenAlex now limits requests without `OPENALEX_API_KEY` to a daily budget shared by every client on one IP address; one search costs 10 of 1000 credits, and an exhausted budget answers 429 with a Retry-After of several hours.
OpenAlex therefore cannot be the only fallback.

## Decision

1. The default cascade queries tiers in order: Semantic Scholar; then OpenAlex and arXiv together; then CrossRef, OpenLibrary and Clio, one at a time.
   Backends in one tier are queried concurrently and their results interleaved by rank.
   A result is on topic when its title, abstract or authors contain at least half of the query's topic words.
   A tier with an on-topic result ends the cascade.
   When no tier has one, the answer is every tier's results, in tier order.
2. arXiv is searched through `export.arxiv.org/api/query` with the OR of every pair of the query's topic words, each pair ANDed across all fields, sorted by relevance.
   A paper must match two words, and papers matching more pairs rank higher.
   ANDing every word returned nothing for the five-word query; ORing single words ranked Shapley-only papers first.
3. Topic words are the query's lowercase alphanumeric tokens minus a fixed stopword list; a trailing plural `s` is ignored in matching.
   On-topic results rank before the rest, and each group keeps the backends' own order.
   Ranking by the share of topic words instead put papers matching "stochastic rate uniform distribution" above arXiv's dropout papers for "stochastic dropout rate uniform distribution": a count of matched words weights a common word like a rare one, and the backends' relevance scores do not.
   Each backend is asked for at least 25 results so the floor chooses from more than a short `--limit`.
   `lit add <title>` resolves to the first result with the largest share of topic words in its title and authors, as the cascade's title ranking did before.
4. Records of one work are merged on the strongest identifier both carry: DOI, then arXiv id, then normalized title.
   The higher ranked record and its source are kept, and fields it lacks are filled from the other.
5. Every `--json` search result carries `source`: `s2`, `openalex`, `arxiv`, `crossref`, `dblp`, `openlibrary`, `philpapers` or `clio`, matching the labels of `lit refs` and `lit cites`.
6. Within the default cascade a request sleeps at most 9s across retries; a Retry-After beyond that ends its retries at once.
   An explicit `--source` keeps the 60s budget of ADR-004, because it has no fallback.
   In patient mode a Semantic Scholar request instead waits up to its patience budget, 300s by default, before the cascade moves on (ADR-012).
7. A search that finds nothing while some source failed exits nonzero, naming every source consulted with its error or "no results".
8. OpenAlex requests carry no `mailto`; a key in `OPENALEX_API_KEY` is the supported way to a private budget.
9. `--source arxiv` selects arXiv alone.

## Consequences

With Semantic Scholar rate limited, a free-text query waits about 9s, then answers from OpenAlex and arXiv; once the OpenAlex budget is spent, from arXiv alone, with OpenAlex's error printed as a note.
Search results no longer re-sort by title overlap and citation count, so the backends' relevance order decides within each group.
Counting abstracts lets title and abstract sources end the cascade more often than the old 30% title overlap did, so a book title that Semantic Scholar matches loosely may no longer reach OpenLibrary; `--source book` reaches it directly.
Searches whose sources all failed now fail scripts that previously received an empty list with exit 0.
