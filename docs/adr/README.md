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

ADR-001 is not in this log.
It governs the boundary between `lit` and the repository that embeds it, so it belongs to the repository containing both, at `docs/ADR-001-lit-and-repository-workflow.md` in the parent.
A record that binds two repositories cannot live in one of them.

## Known gap

No record covers where `lit` keeps its state.
`DESIGN.md` has a "Where state lives" section asserting config precedence, path resolution and database creation as MUSTs, and no decision here supports any of it.
That section is either a decision nobody wrote down or prose nobody agreed to, and until a record settles which, its invariants are unenforceable.
