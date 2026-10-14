# ADR-006: `lit verify` checks named keys on request and reports a rate limit apart from an absent record

## Status

Proposed.

## Context

`lit verify` checked every entry of a bibliography.
A caller that needed about 18 entries of a 609-entry file checked them all, exhausted the arXiv and Semantic Scholar rate limits, and got 270 "not found" results, of which 250 were requests still answered with HTTP 429 after their retries.
The report printed both cases as `x key: title`, so a caller could not tell an entry no source has from an entry no source was able to answer for.
`--json` was accepted and ignored by `lit verify` and `lit add`.

## Decision

1. `lit verify <bib> --key K [--key K ...]` checks only the named entries, in file order.
   A key with no entry, or whose entry is marked `% lit:skip`, is an error before any request.
2. With `--json`, `lit verify` prints one array with a record per checked entry: `key`, `status` and `detail`.
   `status` is `ok`, `mismatch`, `book`, `rate_limited` or `not_found`.
   An entry no source verified is `rate_limited` when any source was still answering HTTP 429 after its retries, because that source might have found it, and `not_found` otherwise.
   `detail` is what the human report prints for the entry, plus the lookup errors for an unverified one.
3. With `--json`, `lit add` prints one object: `entry_key`, `bib_file`, and `added`, which is true when the key was new to the file and false when an existing entry was replaced.
4. Without `--json`, both commands print what they printed before, and `lit verify` still exits non-zero when any entry is unverified or mismatched.

## Consequences

A caller can retry exactly the `rate_limited` keys later instead of rechecking the file.
A 5xx or connection failure on every source still reports `not_found`, with the failures in `detail`; only a rate limit is singled out, because only it is known to be transient.
