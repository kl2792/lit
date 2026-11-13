# ADR-010: proceedings PDFs come from a host rule table, and a title read falls back to open-access copies

## Status

Proposed.

## Context

Many machine learning papers appear only in open-access proceedings: NeurIPS, PMLR, JMLR, ACL Anthology, CVF, ECVA, IJCAI, AAAI and OpenReview.
Such a paper often has no DOI and no arXiv id.
Ba and Frey's "Adaptive dropout for training deep neural networks" (NeurIPS 2013) is one.
`lit read` fetched only arXiv ids and DOIs, and ADR-005 rejected every URL outside its five web hosts, so `lit read https://proceedings.neurips.cc/paper/2013/file/7b5b23f4aadf9513306bcd59afb6e4c9-Paper.pdf` failed with "unsupported host" and `lit read "Adaptive dropout for training deep neural networks"` failed with "not found locally".

The proceedings hosts differ from ADR-005's hosts in what they serve.
Each paper has a PDF, which needs no text extractor, and most hosts also serve a landing page whose `citation_*` meta tags the publisher maintains for Google Scholar.
The PDF URL and the landing URL of a paper follow a fixed pattern on most hosts, so one can be computed from the other without a request.

## Decision

1. `lit read <URL>` accepts URLs on the hosts of one table in `src/api/proceedings.rs`.
   Each rule names its hosts and a list of URL forms.
   A form lists the path templates it accepts and the templates of the canonical PDF URL, the canonical landing URL or the DOI it yields; `{name}` matches one path segment and `{name*}` any run of characters.
   Adding a host means adding a rule and a mapping test.
   A URL on no listed host keeps ADR-005's behavior, and the error names both host lists.
2. A URL yields a PDF URL, a landing URL or both.
   When the landing URL is known, its `citation_title`, `citation_author`, `citation_publication_date` (or `citation_date`), `citation_doi` and `citation_pdf_url` tags give the metadata.
   A `citation_pdf_url` that a rule canonicalizes replaces the computed PDF URL, so the spellings of one paper share an artifact.
   When neither the rule nor the tags give a PDF URL, the first link ending in `.pdf` on the landing page that a rule of the same host accepts is used.
   A landing page without `citation_title` gives no metadata; then the artifact records none and is named `<rule id>-<PDF file stem>`, since ADR-002 forbids inventing metadata.
3. A `dl.acm.org` URL names its DOI, so `lit read` reads that DOI through the existing DOI path.
4. The PDF must start with `%PDF`.
   A host that answers with a bot challenge, as OpenReview, AAAI and DBLP did from Columbia's network on 2026-11-13, fails with the fetch error and nothing is written.
5. The artifact is `etc/pdf/<citekey>/` holding `paper.pdf`, its `pdftotext` extraction `paper.txt`, and a `source.yaml` that records `url` (the canonical PDF URL), `landing_url`, `host` (the rule id), `source_url`, `doi` when the landing page gives one, `metadata_source` and `metadata_confirmed`.
   The directory is claimed as in ADR-005 decision 3.
   A later read of any URL whose canonical PDF or landing URL matches a recorded `url` or `landing_url` reads the artifact with no request, and `--no-cache` refetches into the same directory.
6. `lit read <query>` for a query that matches no local artifact and is a Semantic Scholar id (40 hex digits or `CorpusId:<n>`) or a title (any query containing whitespace) tries, in order:
   1. Semantic Scholar: the paper for the id, or the `paper/search/match` result for the title, accepted only when its title equals the query after lowercasing and removing punctuation.
      Its arXiv id or DOI is fetched as `lit read` fetches ids; otherwise its `openAccessPdf` URL is fetched, through the rule table when a rule accepts it and otherwise as a PDF with Semantic Scholar's metadata.
   2. OpenAlex: the title search result whose title equals the title, and each of its location URLs that a rule accepts.
   The first source that stores an artifact wins.
   When none does, the error names the local artifact directory and each source tried with its outcome.
   A one-word query is taken for a mistyped local name or citekey, so it fails as before, with no request.

## Consequences

`lit read` can fetch any paper whose PDF or landing page is on a listed host, and any paper with a title match on Semantic Scholar or OpenAlex that points to an open-access copy.
A title that matches no local artifact now costs up to two metadata requests before it fails.
A one-word title cannot reach the fallback; its Semantic Scholar id or a URL can.
Hosts behind bot challenges stay listed so their URLs map and fail with the host's answer rather than "unsupported host"; reading them needs the PDF saved by hand and `lit misc --pdf`.
A host that changes its URL layout breaks its rule; the mapping tests use recorded URLs, so only a live read detects that.
Old `papers.nips.cc/paper/<n>-<slug>` URLs have no computable canonical form, so a read through one stores under the canonical PDF URL only when its landing page supplies `citation_pdf_url`.
