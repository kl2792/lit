# ADR-008: Deterministic main TeX selection

## Status

Accepted for implementation.

## Decision

`lit read` uses the first usable source with usage `toplevel` in the `sources` array of `00README.json`, preserving array order.
A manifest source is usable if its path resolves to an existing file inside the archive directory after resolving `..` components and symlinks.
If it finds no usable manifest source, it examines only top-level `.tex` files.
It recognizes a document declaration when reading a candidate file as UTF-8 succeeds and the resulting string contains `\documentclass` or `\documentstyle`.
Among candidates with a recognized declaration, it prefers `main.tex`, then `paper.tex`, then the first filename in lexical order.
Only when no candidate has a recognized declaration does it fall back to `main.tex`, then `paper.tex`.

## Rationale

An archive can include both a manuscript and complete publisher templates.
Filesystem enumeration order does not identify the manuscript and can vary across extractions.
Conventional root names provide a generic preference without recognizing a particular paper or publisher.
During fallback scanning, a `main.tex` fragment without a recognized declaration cannot displace an unconventional file with a recognized declaration.
The manifest remains authoritative, including for roots in subdirectories.

## Verification

Regression cases cover bundled templates, conventional name precedence, unconventional roots beside fragments, lexical ties, manifest precedence, and existing fallback behavior.
This heuristic does not establish that an arbitrarily named document root is the manuscript when several complete documents have no manifest or conventional root name.
