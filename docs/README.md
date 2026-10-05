# lit documentation

## Where a fact goes

Every fact about `lit` lives in exactly one place, and that place is decided by who can check it.

- If `clap` holds it, `lit --help` is the reference and no document restates it. Flag names, argument counts and defaults drift the moment a document copies them.
- If a test holds it, the test is the reference. Prose that restates an assertion becomes false the first time the assertion changes and nothing fails.
- Everything else is hand-written, and that is what this folder is for: why a command exists, how commands compose, what invariants hold across them, and which decisions were made on purpose.

A document here that could be replaced by `--help` output or a test name is drift waiting to happen.
Delete it rather than maintain it.

## The documents

| Document | Holds | Read it when |
|----------|-------|--------------|
| [`DESIGN.md`](DESIGN.md) | Architecture, where state lives, the invariants that span commands | You are changing how `lit` stores or resolves anything |
| [`WORKFLOWS.md`](WORKFLOWS.md) | Task recipes composing several commands | You want to get something done |
| [`CLIO.md`](CLIO.md) | The Columbia catalog integration: sync, auth, the local index | You are working on `lit clio` or `lit download`'s library tier |
| [`adr/`](adr/) | Architecture decision records: what was decided, what was rejected, and why | You are about to undo a decision, or wondering why something is shaped the way it is |
| [`reference/`](reference/) | External-format notes that are not decisions | You are parsing something someone else specified |

The top-level [`README.md`](../README.md) is the entry point for someone who has never run `lit`.
It holds the synopsis and installation, and points here for everything else.

## Architecture decision records

An ADR is written when a choice was not forced, so that the next person can tell a deliberate decision from an accident.

- [`adr/ADR-002-artifact-acquisition-and-provenance.md`](adr/ADR-002-artifact-acquisition-and-provenance.md): recorded facts over re-derivation, covering catalog DOI lookup, the acquisition tier order, artifact provenance, failure typing and BibTeX sanitization.
- [`adr/ADR-003-where-lit-keeps-its-state.md`](adr/ADR-003-where-lit-keeps-its-state.md): where `lit` keeps its state, one resolver per path, each an environment override, a stated default, and an error.

ADR-001 is not here.
It governs the boundary between `lit` and the repository that embeds it, so it lives in the parent repository at `docs/ADR-001-lit-and-repository-workflow.md`.
A record spanning both repositories belongs to the one that contains both.

## Reference notes

- [`reference/marc-umb.md`](reference/marc-umb.md): MARC field and subfield notes for the catalog records `lit clio sync` parses.
