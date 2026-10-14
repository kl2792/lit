# ADR-007: An artifact directory belongs to the paper its `source.yaml` names, and a refetch replaces files in place

## Status

Proposed.
Supersedes the ADR-002 rule "arXiv auto-download fires only on `NotFound`".

## Context

`lit download` named an artifact directory `<lastname><year><titleword>` and wrote into it whether or not it already held another paper.
Two papers with one generated slug were extracted into one directory: `wang2025does` held both arXiv 2502.11447 and 2510.03659, and `lit read` returned the first paper's text for the second id.
`lit read` matched a query against directory names before reading any `source.yaml`, so it could not tell the two apart.
A fetch that failed after creating its directory left one holding only `source.yaml`, which `lit read` then reported as unreadable on every later call, because ADR-002 fetched only on `NotFound`.
After a fetch, `lit read` searched for the directory again by query instead of using the one the fetch had written, which discards a known fact (ADR-002).
Two concurrent fetches staged the e-print at the same path inside the target directory and could delete each other's copy.
The first fix for concurrent fetches, a `.lit-claim` file, outlived any download that crashed while holding it, and that name was then skipped by every other paper.

## Decision

1. **Ownership is identity.**
   A directory under `etc/pdf` belongs to the papers its `source.yaml` names by `arxiv` or `doi`; DOIs compare case-insensitively, and an arXiv DOI (`10.48550/arXiv.<id>`) names the arXiv id.
   One function, `owner_dir(base, id)`, finds a paper's directory for both downloads and `lit read`.
   When several directories name one id, both take the first by name and print a warning naming all of them; neither errors, because the duplicate is an earlier defect the user can merge by hand.
2. **Lookup order.**
   For an arXiv id or DOI, `lit read` tries `owner_dir` first and only then a directory-name match, and a name match never returns a directory naming another paper.
   Any other query matches names, then `source.yaml` text, as before.
   Dot-prefixed directories, which are staging areas, never match.
3. **Names and suffixes.**
   A download of a paper with no directory takes the first of `slug`, `slug`b, ..., `slug`z, `slug`27, ... that is free: absent, or holding no content and naming no paper.
   Dotfiles such as `.DS_Store` are not content, so a directory Finder touched is still free.
4. **Claims are crash-safe locks.**
   A download claims a free name by taking its lock, the directory `.lit-lock` holding `owner.json` with the paper id, process id, host and start time in Unix seconds.
   It writes `owner.json` into a private `.lit-lock.tmp-<pid>-<nanos>-<seq>` and renames that onto `.lit-lock`; the rename fails onto a non-empty directory, so only one process takes the lock, and no process ever reads a lock without its owner.
   A live lock for the same paper is shared, and one for another paper sends the download to the next name.
   A lock is stale when its process no longer exists on its host, or when it is older than one hour; a stale or unreadable lock is broken by renaming it to `.lit-lock.stale-<pid>-<nanos>-<seq>` and deleting that, after which the breaker retries.
   The rename lets one breaker move the lock, and a breaker that finds it moved by another simply retries; a breaker that moved a lock which turns out live, because another breaker took it in between, puts it back.
   Releasing the lock deletes it only if `owner.json` still names this holder, and then deletes the directory if it is empty, so a failed first fetch leaves nothing behind.
5. **Staging and placement.**
   An e-print is unpacked into a sibling `.lit-staging-<name>-<pid>` and its entries are then moved into the directory, each replacing the same-named entry; entries the e-print lacks are kept.
   A failed unpack deletes only the staging directory, so the existing `source.yaml` and any hand-attached file survive it.
   `source.yaml` is written only after the e-print is in place.
   `lit misc` and web-page artifacts stage the same way and publish by renaming the staging directory over the target, keeping the old copy as `.lit-staging-<name>-<pid>.old` until the rename succeeds.
6. **Refetch triggers.**
   `lit read` fetches a paper when no directory matches and the query is an arXiv id or DOI, when the matched directory holds only `source.yaml` and dotfiles, and when `--no-cache` is set and the query or the directory's `source.yaml` names a paper.
   A fetch returns the directory it wrote, and `lit read` reads that directory; a fetch that delivers nothing is an error.
7. **`--no-cache` means refetch.**
   The global `--no-cache` flag keeps both meanings: bypass the HTTP response cache, and for `lit read` refetch a readable copy.
   On success a refetch overwrites a hand-attached `paper.pdf` when the fetch delivers a PDF; that is the point of asking for a fresh copy.
8. **Provenance after a refetch.**
   `source_url` and `retrieved` always come from the latest fetch, because they describe the bytes on disk.
   The bibliographic fields and `metadata_confirmed` of a confirmed record survive a refetch that found no provider metadata, and `bibtex_key` carries over.
9. **Output.**
   Download progress goes to stderr, so `lit read` prints only the path on stdout and `lit-mcp`'s JSON-RPC stream on stdout carries only protocol messages.
   `lit download --url-only` still prints its URLs on stdout, since they are its output.
10. **Crash debris.**
    Lock scratch directories and staging directories follow the lock's staleness rule, read from the pid in their name, assumed to be on this host, and from their modification time.
    Claiming a name deletes the stale lock scratch directories inside it, and creating a staging directory deletes the stale staging directories of the same artifact, found by name in one listing of `etc/pdf` that opens no file.
    `lit check` reports every stale lock, lock scratch directory and staging directory under `etc/pdf`, and every `.lit-claim` file the retired claim scheme left; `lit check --fix` deletes them, and then any artifact directory they leave empty.
    `lit check` never reconciles a dot-prefixed directory as an artifact, since its `source.yaml` describes an unfinished write.

## Alternatives rejected

### A separate `--refetch` flag

It would leave `--no-cache` on `lit read` meaning only "bypass the HTTP cache", which still returns the stale local copy and so does nothing a user asking for a fresh copy wants.
A second flag would be one more name for the same intent.

### Erroring when two directories name one paper

Every `lit read` of that paper would then fail until a human merged the directories, while either directory is a usable answer.

### Claiming with `create_dir` alone

`create_dir` is atomic only for a name that does not exist yet, and a free directory may already exist empty or hold `.DS_Store`.
Treating every existing directory as taken would skip those, and treating it as free would let two papers claim it at once; the lock directory is atomic in both cases.

### A claim file created with `create_new`

This was the first claim scheme, `.lit-claim`.
`create_new` makes the file before its id is written, so a concurrent reader could find it empty and take it for another paper's claim, and a download that crashed left a claim that turned every other paper away from the name for good.

### Publishing by renaming the staging directory over the target

It is atomic, but it replaces the whole directory and so deletes hand-attached files and the existing `source.yaml`.

### An index of ids to directories

Identity lookup reads every `source.yaml` under `etc/pdf` once per query.
An index would be a second record of ownership that can disagree with the files, and the scan is cheap enough not to need one at the current corpus size.

## Consequences

A slug collision yields a suffixed directory, and `lit read` returns the right paper for each id.
A failed fetch leaves no metadata-only directory, and a failed refetch leaves the previous copy intact.
A crashed download leaves a lock that the next claim on the name breaks once the process is gone, and debris that the next write of the same artifact or `lit check --fix` deletes.
A download still running after one hour loses its lock to the next claim on the name; its release then leaves the new holder's lock in place.
Staleness by process id assumes one host per `etc/pdf`: on a directory shared between hosts, a live staging directory of another host can be deleted when its pid is unused here, so only one host may run lit on it at a time.
A refetch does not delete files the new e-print lacks, so a renamed file in a new version leaves the old one beside it.
Directories created before this record that hold two papers keep doing so until refetched by hand.
