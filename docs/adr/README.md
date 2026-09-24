# Decision log

This folder is the log.
Everything else in `docs/` is a derived view of it.

## Why a log

`lit`'s documentation kept going stale in a specific way.
A claim would be written directly into a reference document, nothing would ever have agreed to it, and the code would drift away from it with nothing to notice.
`DESIGN.md` asserted three MUST-level invariants about where state lives that no decision ever made, and shipped code violated all three.
A claim nobody decided is a claim nobody can be held to.

So decisions are appended here, and reference documents restate them.
The log is the source of truth.
A reference document is a rendering of the log for a reader who wants current state rather than history.

## Rules

- **A record is immutable once accepted.** While its Status is Proposed it is a draft and may be edited freely. When it becomes Accepted it freezes. Changing your mind afterwards means appending a new record that supersedes it, never editing the old one. The old decision was really made, and a reader tracing why the code looks this way needs to see it.
- **Numbers are never reused and never renumbered.** A superseded record stays in place with its status updated to point at its successor. That status line is the one permitted edit, because it is a pointer into the log rather than a change to what was decided.
- **Current state never lives in a record.** Defect lists, test counts, what is fixed today: all of it changes without any decision being made, so none of it belongs in the log. It belongs in a derived view, or in the tests.
- **Every normative claim in a derived view cites the record that decided it.** A MUST in `DESIGN.md` with no citation is a finding against the log: either a decision was made and never recorded, or nobody ever agreed to it.
- **The log must replay.** The normative content of the derived views should be reconstructible by reading these records in order. If a derived view says something no record supports, one of the two is wrong.

## What is a decision

A record is written when a choice was not forced: when a competent person could have chosen otherwise and the reason they did not is worth keeping.

Not every change needs one.
A bug fix that restores stated behavior decides nothing; the test that pins it is the record.
A change that alters what the tool promises, or that closes off an alternative someone will later want to reopen, needs a record.

## The records

| Record | Decides | Status |
|--------|---------|--------|
| [ADR-002](ADR-002-artifact-acquisition-and-provenance.md) | Recorded facts over re-derivation: catalog DOI lookup, acquisition tier order, artifact provenance, failure typing, BibTeX sanitization | Proposed |
| [ADR-003](ADR-003-where-lit-keeps-its-state.md) | Where `lit` keeps its state: one resolver per path, each an environment override, a stated default, and an error | Proposed |

ADR-001 is not in this log.
It governs the boundary between `lit` and the repository that embeds it, so it belongs to the repository containing both, at `docs/ADR-001-lit-and-repository-workflow.md` in the parent.
A record that binds two repositories cannot live in one of them.

## Known gaps

These are normative claims in the derived views that no record decides.
Each is a finding against the log under the citation rule above.

- `docs/DESIGN.md` requires callers to use the `entry_key` returned by `lit add` and `lit misc` and never to construct a citekey heuristically.
  Canonicalisation is `lit`'s, so ADR-001 leaves the rule here, and nothing here states it.
- `docs/DESIGN.md` requires every environment variable it names to be read by the code.
  The rule earned itself: `LIT_CACHE_DIR` was documented in three files and read by none.
  It is a claim a test could hold, so what it needs is that test rather than a record.
- `docs/DESIGN.md` requires the unbuilt `lit author` command to surface OpenAlex record merges rather than present a merge as one work.
  A requirement on a command that does not exist has nothing to check it, and it becomes decidable when the command is specified.
