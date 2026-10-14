# ADR-005: web pages are read only from an allowlist of hosts, each with its own extractor

## Status

Proposed.

## Context

Much interpretability work is published only as web pages: Alignment Forum and LessWrong posts, Distill articles, and Transformer Circuits Thread articles.
These have no arXiv id, and of the three only Distill assigns DOIs, so `lit read` could not fetch their text and `lit add` could only reach them by searching for a page title.

A generic scraper would accept any URL, take the `<title>` as the title, guess authors from the byline, and keep whatever text survives a boilerplate filter.
On these hosts that fails in specific ways.
A forum post page is rendered by JavaScript, so the static HTML holds no post body.
The Transformer Circuits template ships an empty `authors` list in its front matter and names the authors only in the rendered byline.
A Distill page is about 15 MB of HTML with inline figures, and its article text is interleaved with a table of contents, figure labels, an appendix and a footer that a boilerplate filter would have to recognize.
A scraper that guesses would write plausible but wrong authors into a bibliography, which ADR-002 forbids: metadata is recorded from a source or not at all.

## Decision

1. `lit read <URL>` and `lit add <URL> <bib>` accept URLs on five hosts: `alignmentforum.org`, `lesswrong.com`, `greaterwrong.com`, `distill.pub` and `transformer-circuits.pub`.
   Any other host is an error that names the five; there is no generic fallback.
   Adding a host means adding an extractor and fixture tests for it.
2. Each host family reads metadata from the source its publisher maintains.
   Forum posts come from the LessWrong GraphQL API, which serves Alignment Forum and GreaterWrong posts under the same post id, and give the title, `postedAt`, the poster and coauthors, and the HTML body.
   Distill pages give `citation_*` meta tags, including the DOI.
   Transformer Circuits pages give the title, the byline author names and the published date.
   The body is the page's `d-article`, `article` or `main` element, rendered to plain text without navigation, scripts, styles, figures' vector graphics, comments, appendices or footers; math, citations and footnotes stay inline.
3. A page is stored as the artifact `etc/pdf/<citekey>/` holding `paper.txt` and a `source.yaml` that records the canonical URL, the host, the fetch time, the extraction method and `metadata_confirmed`.
   The artifact is the cache: a later request for any spelling of the same page, or for the same post on another forum host, reads it with no request.
   `--no-cache` refetches into the same directory.
   The page body never enters the HTTP response cache.
4. `lit add` takes the BibTeX entry from the DOI path when the page has a DOI, and otherwise writes `@misc` with the title, authors, year, the site's name as `howpublished`, and the URL.
5. Requests go through `http::Client`, so the retry and Retry-After rules of ADR-004 apply unchanged.

## Consequences

A page on an unlisted host cannot be read or added from its URL; the user saves the text or runs `lit misc` by hand.
A redesign of a host's page template can break its extractor, and the fixture tests do not detect that, because they run against stored samples; the live smoke test is the check.
Forum metadata names authors by display name, which is often a username, so a citekey can come from one (`lawrencec2022causal`), and `--key` overrides it.
Web artifacts are not indexed in the paper database, so `lit search --local` does not find them; the artifact scan that finds them costs one `source.yaml` read per artifact directory.
