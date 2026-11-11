# ADR-009: Publisher supplementary artifacts

## Status

Proposed. An isolated implementation exists; independent review and integration are pending.

## Contract

`lit download DOI --supplements --url-only --json` discovers links explicitly declared by a supported publisher for that DOI.
The initial resolver supports Frontiers article HTML only.
Unsupported publishers, an unrecognized page, a denied response, and a recognized page with no supplementary links have distinct results.
Successful discovery returns publisher labels, declared URLs and response metadata in JSON.
Landing bytes persist only with `--capture-discovery PATH`, which writes `landing.html` and `response.json` under that path.
The capture replaces credential-shaped strings with same-length placeholders so it can be committed as a fixture; `response.json` then hashes the stored bytes and counts the replacements in `credential_redactions`.
It never guesses supplementary filenames.

An explicit `--supplements-page URL` may name a PMC article page when the publisher route does not expose the needed files.
The page must confirm the requested DOI and declare the supplementary links for that same PMC article.
Discovery accepts only HTTPS on the approved PMC repository host, and resource redirects remain within the approved publisher or PMC repository hosts.
The existing checks of MIME, file signatures, hashes, captures and preservation of the parent article also apply to repository copies.
No repository URL is inferred from a title, filename or unsuccessful publisher request.
`--from-discovery PATH` reuses a prior capture without fetching the landing page again.
The command checks the stored HTML's hash and length, the requested and final page scope, the DOI and the article identity, then extracts and validates the literal asset links again.
The original discovery provenance remains attached to the acquired resources.

`--cloud-metadata PATH` may accompany `--from-discovery PATH` to map verified PMC supplementary links to the official PMC Cloud objects.
The Cloud capture contains `metadata.json` and `response.json`; the command verifies its hash, DOI, PMCID, version, publication type and license before matching literal `media_urls` to the already identified supplementary basenames.
Main article figures remain outside that supplementary selection.
The supported Cloud bucket is `pmc-oa-opendata`, served through HTTPS on `pmc-oa-opendata.s3.amazonaws.com`; every object must remain within the bound article and version prefix.
The supplied MD5 must match each complete object before storage or reuse, while SHA256 remains the stored artifact identity.
Cloud objects served as binary data also require the declared file extension and file signature to agree; this does not relax validation for other publishers.

`lit download DOI --supplements --json` stores validated PDF or raster image artifacts under the existing parent directory's `supplements/` directory.
The main article and its `source.yaml` remain unchanged.
A manifest records the parent DOI and discovery provenance.
Every resource record retains its publisher label, declared, resolved and requested URLs, role and acquisition status.
A fetch attempt records `attempted_at`; a failed acquisition also records `failed_at` and `error`.
A complete received response supplies `final_url`, `http_status`, `mime_type`, `byte_length`, `sha256` and `retrieved_at`, even when HTTP status, validation or storage rejects acquisition.
When a complete body is unavailable, the record retains the last attempt's known final URL, HTTP status and MIME, but omits complete-body length, hash and `retrieved_at`.
A failure before any response omits response fields.
Only a validated artifact successfully stored has `local_relative_path`.
The three timestamps use Unix seconds; `retrieved_at` is the command's timestamp for recording complete-response provenance.
Partial acquisition preserves successes and returns a nonzero status.
Repeated acquisition reuses only artifacts whose recorded hash still matches their bytes.

## Verification plan

Synthetic parser fixtures precede implementation and are labelled as synthetic.
Tests cover relative links, deduplication, labels, unsafe links, missing layout, denial, MIME disagreement and main-file preservation.
Transport tests use local HTTP responses, including redirects and partial failures.
A real Frontiers fixture must be captured through the scoped command transport with response provenance, followed by live discovery and acquisition through Make targets.
Live denial must remain a retrieval failure and cannot establish absence of supplements.

## Captured publisher limitation

On 2026-10-23 the official page for DOI `10.3389/fncom.2020.00006` returned HTTP 200 and named seven supplementary figures.
The HTML did not expose supplementary download anchors.
The publisher XML declared relative URI references `Image_1.jpg` through `Image_7.jpg`, with `Image_4.png` in place of `Image_4.jpg`.
Those references resolve against the XML document URL under XML Base; their presence alone does not establish availability at the resolved URLs.
The implementation reports an unsupported layout for this captured page and does not invent CDN URLs.
The response bytes and provenance are in `test/fixtures/frontiers/`.
Scoped live checks of all seven XML references returned HTTP 404 and HTML bodies at their standard resolved URLs.
The EPUB explicitly linked by the publisher returned HTTP 200 with SHA256 `2aecfa57c96f0ab5ed2a4226bf73d9799a5964e0b7ec6b0dea374f041b0dff16`.
Its 41 archive entries and `OPS/package.opf` manifest contained none of the seven supplementary filenames.
The EPUB's main figure files remain main article assets.
Live acquisition for this article remains unresolved; these checked routes supplied no supplementary image bytes.

The CLI and synthetic discovery regressions failed before implementation.
Acquisition tests were added after the first acquisition implementation, so their passing results do not supply preimplementation regression evidence.
They exercise partial failure, intact-file reuse, corruption refetch, basename collisions, MIME rejection and preservation of the main article.
Deterministic local transport tests retain the allowed HTTPS publisher URLs and use a loopback CONNECT proxy with a fixture TLS certificate trusted only by the test client.
They exercise allowed and forbidden redirects, the redirect limit, HTTP denial, rate-limit retry, header and body timeouts, declared and streamed size limits, truncated responses and partial acquisition.
The tests exposed missing URL context on body and size failures and a missing timeout cause on body failures; the correction preserves those details while continuing to reject incomplete bytes.
The process chronology and unavailable violation-logging tool are recorded in `docs/supplements-process-audit.json`.
