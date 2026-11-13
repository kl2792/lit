# ADR-012: Patient mode waits for Semantic Scholar under a shared request pace before falling back

## Status

Proposed.

## Context

ADR-011 caps each request in the default search cascade at 9s of retry sleep, so the cascade answers quickly from OpenAlex and arXiv.
Without `S2_API_KEY`, Semantic Scholar answers most free-text queries with HTTP 429, so under that cap nearly every unkeyed query is answered by the fallback tier.
A systematic literature review would rather wait for Semantic Scholar, whose relevance ranking and abstracts it relies on, than finish sooner with fallback results.
`lit refs` and `lit cites` have the 60s budget of ADR-004, but its 5 attempts sleep only 2, 4, 8 and 16s, 30 to 45s with jitter, before falling back to OpenAlex.

Parallel agents make the 429s worse: each `lit` process retries on its own clock, so several processes started together send their requests and their retries in bursts.

Semantic Scholar's API page (https://www.semanticscholar.org/product/api, read 2026-11-13) states that unauthenticated requests "are rate-limited to 1000 requests per second shared among all unauthenticated users" and that "The introductory rate limit for an API key is 1 RPS on all endpoints".

ADR-003 rejects a configuration file, so a persistent default cannot live in one.

## Decision

1. Patient mode applies to requests to `api.semanticscholar.org` only; every other host keeps the retry policy it has today.
2. In patient mode a Semantic Scholar request retries 429, 5xx, timeouts and connection errors until its total retry sleep would exceed the patience budget, 300s by default.
   The attempt count is not limited.
   Each wait is the server's Retry-After when it sends one, otherwise `min(2^(k+1)s, 60s)` for retry `k` plus up to 50% jitter: 2, 4, 8, 16, 32, 60, 60, ... seconds before jitter.
   A Retry-After longer than the remaining budget ends the retries at once, because sleeping it out would end in the same fallback.
   When the budget runs out, the caller falls back as it does today: search to the next cascade tier, refs and cites to OpenAlex.
3. Before every Semantic Scholar attempt in patient mode, the process reserves a request slot in a pacing file shared by every `lit` process using the same database: the file `<database path>.s2-pacing`, holding the next free slot in milliseconds since the epoch, updated under an exclusive `flock`.
   Slots are 1.1s apart: 1 request per second is the documented limit for an API key, and the extra 0.1s absorbs differences in when requests leave each process.
   For unkeyed requests the limit is a pool shared with every other client, which pacing cannot control; pacing keeps one machine's agents from sending bursts into that pool.
   The lock is held only to read and write the slot, never while waiting for it.
   A reserved slot more than 10 minutes ahead is treated as stale and replaced.
   Pacing waits do not count toward the patience budget.
   If the pacing file cannot be used, the request proceeds unpaced and says so on stderr.
4. Each retry prints one line on stderr naming Semantic Scholar, the error, the attempt number, the wait, and the patience used so far, for example `note: Semantic Scholar: rate limited (attempt 3), retrying in 8.4s (14s of 300s patience used)`.
   A pacing wait of 2s or more prints one line too, so a waiting call is distinguishable from a hung one.
5. Patient mode is selected, in order of precedence, by the global flags `--no-patient` (off), `--patient` or `--patient-budget SECS` (on, the latter with that budget), and then by the database setting `patient`, which `lit config set patient on` stores in the database's `meta` table.
   `lit config set patient_budget SECS` sets the default budget.
   The setting lives in the database `lit` already opens, so it adds no file, search path or environment variable.
6. Without patient mode, every request keeps the retry policy, attempt limit and stderr text it has today.

## Consequences

A patient search without a key can take several minutes, and the stderr lines show where the time goes.
Agents that share a database share the pace, so ten parallel patient searches send about one Semantic Scholar request per 1.1s between them instead of ten at once; each waits its turn, up to about 11s for the tenth.
Processes using different databases, or running without patient mode, do not share the pace.
A workspace can make every invocation patient with one `lit config set patient on`, and a script that needs the fast fallback passes `--no-patient`.
