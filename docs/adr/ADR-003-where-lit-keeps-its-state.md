# ADR-003: One resolver per path, with an override, a default, and an error

## Status

Proposed.

## Context

`lit` reads and writes state in four places outside the directory the user is standing in: the SQLite database that also holds the response cache, the Clio catalog index, the tree of PDF artifacts under `etc/pdf/`, and the EZProxy cookie file.
Each acquired a resolution as it was written, and no record settled how any of them resolve.

`docs/DESIGN.md` grew a "Where state lives" section asserting MUST-level invariants over that machinery.
No record supported any of them, and shipped code violated three: the database default was derived from `current_exe()`, nothing created the database's parent directory before the database was opened, and the `lit db path` subcommand the section required did not exist.
A fourth claim in the same section was normative about `src/config.rs`, a 620-line `.litconfig` layer that called `toml::from_str` while `toml` appears zero times in `Cargo.toml` and `Cargo.lock`, so the file could not have compiled had `src/lib.rs` declared it.
The section was prose nobody had agreed to, which is why nothing noticed when the code disagreed with it.

Three failures follow from the absence of a decision rather than from any one bug.

`lit` and `lit-mcp` each resolved the database path with a separate copy of the same logic, in `main.rs` and in `bin/lit-mcp.rs`.
Two copies can diverge, and when they do the two binaries open different databases while each is internally consistent, which presents to the user as a library that forgets what the other binary wrote.

`project_root()` walked up from the working directory and returned `"."` when the walk found nothing.
`lit check` then scanned whatever directory the user happened to be standing in, found no artifacts, and printed the same report an intact library produces.
A check that passes because it looked in the wrong place is worse than one that errors, because nothing distinguishes it from a real pass.

`default_output_dir()` walks up for a literal `etc/pdf`, takes no environment override, and falls back to a relative `etc/pdf` when the walk fails.
Where `lit download` deposits a paper therefore depends on where the user was standing when they ran it.

## Decision

### One resolver per path, in the library

Every path `lit` resolves gets exactly one implementation, and that implementation lives in the library rather than in a binary.
`lit` and `lit-mcp` are separate binaries and neither can reach the other's code, so a resolver written inside either one has to be copied into the other, and copies drift with nothing to compare them against.
A path literal likewise appears once, at that resolver, so a change to a default cannot land in one binary and miss the other.

### Every resolution is an override, a stated default, and an error

Each path resolves in the same three steps: the environment variable if it is set, otherwise the default stated below, otherwise an error naming the variable that would fix it.

No path falls back to the working directory or to a bare relative path.
That fallback converts a misconfiguration into a run that looks like a success, which is the failure `project_root()` and `default_output_dir()` both exhibit.
An error costs the user one message; a silent wrong root costs them the belief that their library was checked.

### The paths

The database, including the response cache, resolves from `LIT_DB_PATH`, otherwise to `etc/lit/lit.db` under the executable's grandparent directory.
Both `lit` and `lit-mcp` obtain it from the shared resolver.
The executable-relative default is a deviation from the rule above, carried deliberately and recorded under "Open questions" rather than changed here, because changing it repoints every existing installation's library.

The project root resolves from `LIT_PROJECT_ROOT`, otherwise to the nearest ancestor of the working directory holding a non-empty `etc/pdf/`, otherwise to an error.
An `etc/pdf/` with nothing in it does not satisfy the search.
A fresh clone and a checkout whose artifacts were never fetched both present an empty `etc/pdf/`, and rooting a scan there reports a library of zero artifacts as intact.

The artifact output directory is `etc/pdf/` under the project root as resolved above.
`lit download` derives it from that resolver rather than performing a walk of its own, because a second walk is a second resolver, and two resolvers over the same literal are the shape this record exists to remove.

`LIT_PROJECT_ROOT` is part of the tool's documented surface and is listed wherever the other environment variables are listed.
It names the directory `lit` treats as the project root, and it is how a user runs against a library that is not above them.
`LIT_PROJECT_ROOT=/srv/corpus lit check --fix` reconciles the artifacts under `/srv/corpus/etc/pdf/` from any working directory, including one with no `etc/pdf/` in any parent.

### No configuration file

Path resolution reads environment variables and nothing else.
A configuration file adds a third precedence level, a parser, a search path for the file itself, and a second place where a path can be stated, and `src/config.rs` demonstrated the cost before it was ever reached: a resolver nothing calls reads as the authority when someone changes paths, and its tests pass while the real resolver diverges.

### Diagnosis is a command, not a reading of the source

`lit db path` prints every resolved path together with the source that won, and runs before the database is opened.
An unopenable database is the case it exists to diagnose, so it cannot require one.

## Alternatives rejected

### Fall back to the working directory

This is what the code did.
It makes the tool succeed loudly in the one situation where the user most needs to be stopped, and it makes every resulting report ambiguous between "nothing is wrong" and "nothing was examined".

### Create the missing directory instead of erroring

For the database's parent this is right, because the user named the file and the only question is whether its directory exists yet.
For the project root it is wrong: `lit` would materialize an empty `etc/pdf/` wherever the user happened to stand, and the next run would find that directory and root itself there permanently.

### A resolver per binary

This is the state that produced two copies of the database resolution.
The copies agreed by accident of having been written together, which is not a property anything can hold them to.

### A `.litconfig` file

Rejected with `src/config.rs`.
The variables this record names are few and each pins one path, so the file would buy ordering rules and a parser and save nothing.

## Consequences

`lit` and `lit-mcp` cannot open different databases without someone writing a second resolver, which is a visible change rather than a drift.
A user running `lit check` outside a library gets an error naming `LIT_PROJECT_ROOT` instead of a clean report over an empty scan.
A user with a library somewhere other than above their working directory gains a way to say so, which they did not have.

The cost is that `lit` now refuses to run in cases where it previously produced output.
That output was wrong, so the refusal is the point, but it is a behavior change for any caller that ran `lit check` from outside the project and read exit code zero as a pass.

### Open questions

Where the database default should point once it stops deriving from the executable, either `$XDG_DATA_HOME/lit` falling back to `~/.local/share/lit`, or the in-repo `etc/lit/`.
The first is conventional and survives a rebuild from any checkout; the second keeps a research workspace self-contained and backed up with the repository.
Until this is settled, `LIT_DB_PATH` is the way to pin an absolute path.

Two paths are not yet brought under the rule above, and this record does not rule on either.
The Clio catalog index takes `LIT_CLIO_DB_PATH` and otherwise walks up for the nearest `etc/lit/`, so its default is working-directory dependent.
The EZProxy cookie file has no override at all and resolves only by walking up for `.cache/lit/clio/cookies.txt`.
Both are read-mostly caches whose absence is reported rather than silently tolerated, which is why they are lower stakes than the project root, and neither has a demonstrated failure attached to it yet.

## Evidence

`src/config.rs` could not compile: `toml` appears zero times in both `Cargo.toml` and `Cargo.lock`, and the file calls `toml::from_str`.
The same evidence condemned `src/api/s2_dump.rs` with `duckdb` in place of `toml`.

The divergent-database failure is attested by the two copies of the resolution in `main.rs` and `bin/lit-mcp.rs`, which is a mechanism rather than a measurement.
An earlier account of that incident circulated a duration nobody remeasured, and it is deliberately not carried here.
