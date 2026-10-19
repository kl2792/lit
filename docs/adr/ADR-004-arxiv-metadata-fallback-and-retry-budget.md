# ADR-004: arXiv metadata falls back across providers, and retries share one time budget

## Status

Proposed.

## Context

`export.arxiv.org/api/query` can answer HTTP 429 for hours.
`lit read <arXiv id>` and `lit add <arXiv id>` used it as their only metadata source, and `lit download --source` fetched metadata before the e-print, so a throttled metadata API blocked a download from `arxiv.org/e-print/`, a separate endpoint that often still serves.

ADR-002 set retries to 4 attempts with fixed 1s, 2s, 4s waits and capped any Retry-After at 5s.
A server that asks for 30s and gets 5s answers 429 again, so the cap spent the attempts without letting the limit clear.

## Decision

1. Every HTTP GET through `http::Client` retries timeouts, connection errors, 429 and 5xx.
   The wait before retry `k` is the server's Retry-After in delay-seconds form when present, otherwise `2^(k+1)` seconds plus up to 50% jitter.
   Total sleep per request is capped at 60s; a wait that would exceed the cap ends the retries at once.
   The default attempt count is 5, still overridable by `LIT_MAX_ATTEMPTS`.
   This supersedes the retry schedule in ADR-002.
2. Metadata for an arXiv id comes from the first source that returns a titled record: the arXiv API, then Semantic Scholar's `arXiv:<id>` record, then OpenAlex's record for the DOI `10.48550/arXiv.<id>`.
   A fallback prints one line naming its source, and the artifact records it as `metadata_source`.
   A response is cached only once it parses into a titled record, so an HTTP 200 feed with no entry does not stand in for the paper's record for the cache lifetime.
3. Downloads do not depend on metadata.
   When no source answers, the e-print or PDF is still fetched into `etc/pdf/<arxiv id>/`, and `source.yaml` records only the id and provenance, without `metadata_confirmed`, so `lit check --fix` can fill it later and nothing is fabricated.
   `lit add` still fails when no source answers, because a BibTeX entry without a title and authors is fabricated metadata.

## Consequences

A persistently throttled arXiv API costs one 30 to 45s backoff before the fallback answers.
Other providers inherit the honored Retry-After, so a Semantic Scholar 429 can wait longer than before but no longer re-hits the limit early.
Fallback records can spell authors differently from arXiv (for example, Semantic Scholar's "Bei-Ming Liu" for arXiv's "Beiming Liu"); the citekey uses the surname and is unaffected.
